import { test, expect, devices, request as playwrightRequest } from '@playwright/test';
import type { APIRequestContext } from '@playwright/test';
import {
  adminToken,
  createCustomer,
  createEmployee,
  createInquiry,
  patchInquiry,
  setInquiryItems,
  assignInquiryEmployee,
  deleteInquiry,
  deleteEmployee,
  workerOtpLogin,
  injectWorkerAuth,
  injectAdminAuth,
  type WorkerSession,
} from './worker-helpers';

/**
 * FULL SPEC — a worker opens an assigned move, sees everything they need, logs
 * their hours once, and those hours show up read-only in the admin dashboard's
 * employee management page.
 *
 *   what the worker must see on the job:
 *     • the planned START time (08:30) — and NOT an end time
 *     • the addresses (Auszug → Einzug)
 *     • the notes (office hint + crew note)
 *     • the furniture list, if available (Sofa, Umzugskarton)
 *   then:
 *     • the worker logs their hours once (07:00–15:30 = 8.5 h)
 *     • the admin sees those self-reported times read-only at /admin/employees/{id}
 *
 * The worker half runs on an emulated phone (touch); the admin half runs in a
 * desktop context (the dashboard table is desktop-first).
 */
// Pin the timezone so the HH:MM the worker types maps to the same HH:MM the
// admin dashboard renders back (both convert through local time).
test.use({ ...devices['Pixel 5'], channel: 'chrome', video: 'off', timezoneId: 'Europe/Berlin' });
test.describe.configure({ retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';

const OFFICE_NOTE = 'Bitte pünktlich. Schlüssel beim Hausmeister.';
const CREW_NOTE = 'Du fährst den 7,5-Tonner.';
const CLOCK_IN = '08:00';
const CLOCK_OUT = '17:00';
const BREAK_MIN = '30';

let api: APIRequestContext;
let token: string;
let customerId: string;
let employeeId: string;
let inquiryId: string;
let session: WorkerSession;
let jobDate: string;

test.beforeAll(async () => {
  api = await playwrightRequest.newContext();
  token = await adminToken(api);

  const now = new Date();
  jobDate = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-15`;

  customerId = (await createCustomer(api, token)).id;
  const emp = await createEmployee(api, token);
  employeeId = emp.id;

  inquiryId = (await createInquiry(api, token, customerId, jobDate)).id;
  // accept/schedule it + set the planned start time and an office hint
  await patchInquiry(api, token, inquiryId, {
    status: 'scheduled',
    start_time: '08:30:00',
    employee_notes: OFFICE_NOTE,
  });
  // furniture list (admin item editor shape)
  await setInquiryItems(api, token, inquiryId, [
    { name: 'Sofa', volume_m3: 2.5, quantity: 1 },
    { name: 'Umzugskarton', volume_m3: 0.1, quantity: 12 },
  ]);
  // crew assignment (with a per-assignment note)
  await assignInquiryEmployee(api, token, inquiryId, employeeId, CREW_NOTE);

  session = await workerOtpLogin(api, emp.email);
});

test.afterAll(async () => {
  await deleteInquiry(api, token, inquiryId);
  await deleteEmployee(api, token, employeeId);
  // customer hard-delete (best effort)
  await api.post(`${process.env.STAGING_URL || 'http://localhost:8080'}/api/v1/admin/customers/${customerId}/delete`, {
    headers: { Authorization: `Bearer ${token}` },
  }).catch(() => {});
  await api.dispose();
});

test('worker opens the move, sees all details, logs hours once, and they reach the admin dashboard', async ({ page, browser }) => {
  // ---- worker: open the job from the schedule ----
  await injectWorkerAuth(page, session);
  await page.goto(`${FRONT}/worker/schedule`);
  await page.waitForLoadState('networkidle');

  // workers have no digital hours record — the "Stunden" tab must not exist
  await expect(page.getByRole('link', { name: /Stunden/ })).toHaveCount(0);

  const card = page.locator('button.job-card', { hasText: 'Hildesheim' });
  await expect(card).toBeVisible();
  await card.tap();
  await page.waitForURL(/\/worker\/jobs\//, { timeout: 10_000 });

  // start time (planned) — shown; the end time is never rendered
  await expect(page.getByText('08:30 Uhr')).toBeVisible();
  // addresses
  await expect(page.getByText('Teststr. 1')).toBeVisible();
  await expect(page.getByText('31134 Hildesheim')).toBeVisible();
  await expect(page.getByText('Zielweg 9')).toBeVisible();
  await expect(page.getByText('30159 Hannover')).toBeVisible();
  // notes — office hint + crew note
  await expect(page.getByText(OFFICE_NOTE)).toBeVisible();
  await expect(page.getByText(CREW_NOTE)).toBeVisible();
  // furniture list
  await expect(page.getByText('Sofa')).toBeVisible();
  await expect(page.getByText(/Umzugskarton/)).toBeVisible();

  // ---- worker: log start, end and break once ----
  await page.getByLabel('Beginn').fill(CLOCK_IN);
  await page.getByLabel('Ende').fill(CLOCK_OUT);
  await page.getByLabel('Pause').fill(BREAK_MIN);
  await page.getByRole('button', { name: 'Zeiten speichern' }).tap();
  // 08:00–17:00 (9 h) minus the 30-min break = 8.5 h worked
  await expect(page.getByText('8.5 h')).toBeVisible({ timeout: 10_000 });

  // ---- admin: the logged hours show read-only in employee management ----
  const adminCtx = await browser.newContext({ viewport: { width: 1366, height: 900 }, timezoneId: 'Europe/Berlin' });
  const adminPage = await adminCtx.newPage();
  await injectAdminAuth(adminPage, token);
  await adminPage.goto(`${FRONT}/admin/employees/${employeeId}`);
  await adminPage.waitForLoadState('networkidle');

  // the job is on the 15th — switch from the rolling 7-day view to the month
  await adminPage.getByRole('button', { name: 'Monat' }).click();
  await adminPage.waitForLoadState('networkidle');

  // the worker's self-reported start, end and break appear read-only (informational)
  await expect(adminPage.getByText(CLOCK_IN)).toBeVisible();
  await expect(adminPage.getByText(CLOCK_OUT)).toBeVisible();
  await expect(adminPage.getByText(`${BREAK_MIN} Min`)).toBeVisible();

  await adminCtx.close();
});

test('worker enters loose phone-keypad times (8.15 / bare 17 / 45) — they normalize, persist, and reach admin', async ({ page, browser }) => {
  // Movers type on a decimal keypad: "8.15" (not "08:15"), a bare hour "17",
  // etc. Regression: only an exact "HH:MM" was accepted, so these were silently
  // dropped and "Zeiten speichern" looked like it did nothing.
  await injectWorkerAuth(page, session);
  await page.goto(`${FRONT}/worker/schedule`);
  await page.waitForLoadState('networkidle');

  const card = page.locator('button.job-card', { hasText: 'Hildesheim' });
  await expect(card).toBeVisible();
  await card.tap();
  await page.waitForURL(/\/worker\/jobs\//, { timeout: 10_000 });

  // type the way a mover actually types
  await page.getByLabel('Beginn').fill('8.15');
  await page.getByLabel('Ende').fill('17'); // bare hour → 17:00
  await page.getByLabel('Pause').fill('45');
  // moving focus already blurred Beginn/Ende; blur the last field too
  await page.getByLabel('Pause').blur();

  // fields snap to canonical HH:MM on blur
  await expect(page.getByLabel('Beginn')).toHaveValue('08:15');
  await expect(page.getByLabel('Ende')).toHaveValue('17:00');

  await page.getByRole('button', { name: 'Zeiten speichern' }).tap();
  // 08:15–17:00 (8.75 h) minus the 45-min break = 8.0 h worked
  await expect(page.getByText('8.0 h')).toBeVisible({ timeout: 10_000 });

  // reload to prove the loose input was actually saved (not just local state)
  await page.reload();
  await page.waitForLoadState('networkidle');
  await expect(page.getByLabel('Beginn')).toHaveValue('08:15');
  await expect(page.getByLabel('Ende')).toHaveValue('17:00');

  // admin sees the normalized self-reported times read-only
  const adminCtx = await browser.newContext({ viewport: { width: 1366, height: 900 }, timezoneId: 'Europe/Berlin' });
  const adminPage = await adminCtx.newPage();
  await injectAdminAuth(adminPage, token);
  await adminPage.goto(`${FRONT}/admin/employees/${employeeId}`);
  await adminPage.waitForLoadState('networkidle');
  await adminPage.getByRole('button', { name: 'Monat' }).click();
  await adminPage.waitForLoadState('networkidle');

  await expect(adminPage.getByText('08:15')).toBeVisible();
  await expect(adminPage.getByText('17:00')).toBeVisible();
  await expect(adminPage.getByText('45 Min')).toBeVisible();

  await adminCtx.close();
});
