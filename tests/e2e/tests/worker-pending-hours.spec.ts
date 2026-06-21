import { test, expect, devices, request as playwrightRequest } from '@playwright/test';
import type { APIRequestContext } from '@playwright/test';
import http from 'node:http';
import {
  adminToken,
  createCustomer,
  createEmployee,
  createInquiry,
  patchInquiry,
  assignInquiryEmployee,
  deleteInquiry,
  deleteEmployee,
  workerOtpLogin,
  injectWorkerAuth,
  injectAdminAuth,
  API_BASE,
  type WorkerSession,
} from './worker-helpers';

/**
 * PAST-ASSIGNMENT FORCED HOURS LOG.
 *
 * A worker assigned to a job whose day has already passed must log their hours
 * before they can do anything else: on opening the portal a blocking,
 * non-dismissable modal appears. Once logged:
 *   • the assignment disappears from the worker's tab,
 *   • the hours show read-only in the admin dashboard,
 *   • the office gets a Telegram notification ("… hat Stunden erfasst … für den
 *     Auftrag von <Kunde>").
 *
 * We can't move the system clock, so "the day has passed" is simulated by dating
 * the inquiry a few days in the past. The Telegram call is captured by a local
 * mock server the backend is pointed at via AUST_TELEGRAM_API_BASE (see
 * tests/e2e/run-test-backend.sh).
 */
test.use({ ...devices['Pixel 5'], channel: 'chrome', video: 'off', timezoneId: 'Europe/Berlin' });
test.describe.configure({ retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';
const TG_PORT = Number(process.env.TELEGRAM_MOCK_PORT || 8077);

const pad = (n: number) => String(n).padStart(2, '0');

let api: APIRequestContext;
let token: string;
let customerId: string;
let employeeId: string;
let inquiryId: string;
let session: WorkerSession;
let jobDate: string;
let jobMonth: string;

let tgServer: http.Server;
const tgMessages: string[] = [];

test.beforeAll(async () => {
  // Mock Telegram Bot API: record every message the backend posts.
  tgServer = http.createServer((req, res) => {
    if (req.method === 'POST') {
      let raw = '';
      req.on('data', (c) => (raw += c));
      req.on('end', () => {
        try {
          tgMessages.push(JSON.parse(raw).text ?? raw);
        } catch {
          tgMessages.push(raw);
        }
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end('{"ok":true}');
      });
    } else {
      res.writeHead(200);
      res.end('ok');
    }
  });
  await new Promise<void>((r) => tgServer.listen(TG_PORT, '127.0.0.1', () => r()));

  api = await playwrightRequest.newContext();
  token = await adminToken(api);

  // Date the job 3 days ago → the appointment's day has passed.
  const now = new Date();
  const d = new Date(now);
  d.setDate(now.getDate() - 3);
  jobDate = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  jobMonth = jobDate.slice(0, 7);

  customerId = (await createCustomer(api, token)).id;
  const emp = await createEmployee(api, token);
  employeeId = emp.id;

  inquiryId = (await createInquiry(api, token, customerId, jobDate)).id;
  await patchInquiry(api, token, inquiryId, { status: 'scheduled', start_time: '08:00:00' });
  await assignInquiryEmployee(api, token, inquiryId, employeeId, 'Crew');

  session = await workerOtpLogin(api, emp.email);
});

test.afterAll(async () => {
  await deleteInquiry(api, token, inquiryId);
  await deleteEmployee(api, token, employeeId);
  await api
    .post(`${API_BASE}/api/v1/admin/customers/${customerId}/delete`, {
      headers: { Authorization: `Bearer ${token}` },
    })
    .catch(() => {});
  await api.dispose();
  await new Promise<void>((r) => tgServer.close(() => r()));
});

test('a past assignment forces the hours modal; logging clears it from the tab, reaches admin + Telegram', async ({
  page,
  browser,
}) => {
  await injectWorkerAuth(page, session);
  await page.goto(`${FRONT}/worker/schedule`);
  await page.waitForLoadState('networkidle');

  // ---- forced, non-dismissable modal for the overdue job ----
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText('Repro Kunde')).toBeVisible();
  // no escape hatch
  await expect(page.getByRole('button', { name: /Schließen|Abbrechen/ })).toHaveCount(0);

  // ---- log loose-format hours: bare 8 → 16, 30 min break = 7.5 h ----
  await page.getByLabel('Beginn').fill('8');
  await page.getByLabel('Ende').fill('16');
  await page.getByLabel('Pause (Min.)').fill('30');
  await page.getByRole('button', { name: 'Stunden speichern' }).tap();

  // modal closes once everything is logged
  await expect(dialog).toBeHidden({ timeout: 10_000 });

  // ---- reload: the modal must NOT reappear (no longer pending) ----
  await page.reload();
  await page.waitForLoadState('networkidle');
  await expect(page.getByRole('dialog')).toHaveCount(0);

  // ---- the job is gone from the worker's tab for its month ----
  await page.locator('input[type="month"]').fill(jobMonth);
  await page.waitForLoadState('networkidle');
  await expect(page.getByText('Repro Kunde')).toHaveCount(0);

  // ---- Telegram notification fired, naming the employee and the customer ----
  await expect
    .poll(
      () =>
        tgMessages.some(
          (m) => m.includes('Repro Helfer') && m.includes('Repro Kunde') && m.includes('Stunden erfasst')
        ),
      { timeout: 10_000 }
    )
    .toBe(true);

  // ---- admin sees the self-reported hours read-only ----
  const adminCtx = await browser.newContext({
    viewport: { width: 1366, height: 900 },
    timezoneId: 'Europe/Berlin',
  });
  const adminPage = await adminCtx.newPage();
  await injectAdminAuth(adminPage, token);
  await adminPage.goto(`${FRONT}/admin/employees/${employeeId}`);
  await adminPage.waitForLoadState('networkidle');
  await adminPage.getByRole('button', { name: 'Monat' }).click();
  await adminPage.waitForLoadState('networkidle');

  await expect(adminPage.getByText('08:00')).toBeVisible();
  await expect(adminPage.getByText('16:00')).toBeVisible();
  await expect(adminPage.getByText('30 Min')).toBeVisible();

  await adminCtx.close();
});
