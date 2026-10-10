import { test, expect, request as playwrightRequest } from '@playwright/test';
import type { APIRequestContext, Page } from '@playwright/test';
import {
  API_BASE,
  adminToken,
  createCustomer,
  createEmployee,
  createInquiry,
  patchInquiry,
  assignInquiryEmployee,
  setAssignmentHours,
  deleteInquiry,
  deleteEmployee,
  injectAdminAuth,
} from './worker-helpers';

/**
 * HOURS ENTRY ON A FLAKY CONNECTION — Alex enters employee hours from the car.
 *
 * Background (2026-10-09): on a patchy mobile connection, saves stalled, the 15 s
 * client timeout aborted them and the only feedback was a raw "Fetch is aborted"
 * toast. Typed times were then overwritten by the next reload, so he could not
 * tell what had been saved.
 *
 * These tests cut the network with Playwright (route.abort / setOffline) and
 * assert the hours page now:
 *   - keeps the typed value, marks the row red and shows a German message,
 *   - blocks the Stundenzettel export until the row is saved,
 *   - re-saves via "Alle erneut speichern" and automatically when back online,
 *   - retries a failed load transparently,
 *   - steps months with ‹ / › without firing requests for half-typed months.
 */
test.use({ channel: 'chrome', video: 'off', viewport: { width: 1366, height: 900 }, timezoneId: 'Europe/Berlin' });
test.describe.configure({ mode: 'serial', retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';

const MONTH = new Date().toISOString().slice(0, 7); // YYYY-MM
const DAY = `${MONTH}-05`;
const de = (iso: string) => { const [y, m, d] = iso.split('-'); return `${d}.${m}.${y}`; };

let api: APIRequestContext;
let token: string;
let customerId: string;
let employeeId: string;
let inquiryId: string;

test.beforeAll(async () => {
  api = await playwrightRequest.newContext();
  token = await adminToken(api);
  customerId = (await createCustomer(api, token)).id;
  employeeId = (await createEmployee(api, token)).id;
  inquiryId = (await createInquiry(api, token, customerId, DAY)).id;
  await patchInquiry(api, token, inquiryId, { status: 'scheduled' });
  await assignInquiryEmployee(api, token, inquiryId, employeeId, 'flaky network e2e');
  await setAssignmentHours(api, token, inquiryId, employeeId, DAY, {
    clock_in: '08:00:00',
    clock_out: '17:00:00',
    break_minutes: 30,
  });
});

test.afterAll(async () => {
  await deleteInquiry(api, token, inquiryId);
  await deleteEmployee(api, token, employeeId);
  await api.post(`${API_BASE}/api/v1/admin/customers/${customerId}/delete`, {
    headers: { Authorization: `Bearer ${token}` },
  }).catch(() => {});
  await api.dispose();
});

async function openMonth(page: Page) {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/employees/${employeeId}`);
  await page.waitForLoadState('networkidle');
  await page.getByRole('button', { name: 'Monat' }).click();
  await expect(row(page)).toBeVisible();
}

function row(page: Page) {
  return page.locator('tr', { hasText: de(DAY) });
}

async function dbClockIn(): Promise<string | null> {
  const summary = await (await api.get(
    `${API_BASE}/api/v1/admin/employees/${employeeId}/hours?month=${MONTH}`,
    { headers: { Authorization: `Bearer ${token}` } }
  )).json();
  return summary.assignments.find((x: { booking_date: string }) => x.booking_date === DAY)?.clock_in ?? null;
}

const SAVE_URL = `**/api/v1/inquiries/*/employees/*`;

test('a save that fails keeps the value, marks the row and can be retried', async ({ page }) => {
  await openMonth(page);
  const von = row(page).getByLabel('Von');

  await page.route(SAVE_URL, (r) => (r.request().method() === 'PATCH' ? r.abort('internetdisconnected') : r.continue()));
  await von.fill('07:30');
  await von.blur();

  // Readable German error, typed value kept, row marked, banner with retry.
  await expect(page.getByText('evtl. nicht gespeichert').first()).toBeVisible();
  await expect(page.getByText('1 Zeit wegen der Verbindung nicht gespeichert')).toBeVisible();
  await expect(von).toHaveValue('07:30');
  await expect(von).toHaveClass(/border-danger/);
  expect(await dbClockIn()).toBe('08:00:00');

  // The Stundenzettel must not be exported with a missing time.
  await page.getByRole('button', { name: 'Stundenzettel PDF' }).click();
  await expect(page.getByText('Erst die rot markierten Zeiten speichern')).toBeVisible();

  // Connection back → retry from the banner.
  await page.unroute(SAVE_URL);
  await page.getByRole('button', { name: 'Alle erneut speichern' }).click();
  await expect.poll(dbClockIn).toBe('07:30:00');
  await expect(page.getByText('wegen der Verbindung nicht gespeichert')).toHaveCount(0);
  await expect(von).not.toHaveClass(/border-danger/);
});

test('going offline shows a banner and failed times save automatically on reconnect', async ({ page, context }) => {
  await openMonth(page);
  const von = row(page).getByLabel('Von');

  await context.setOffline(true);
  await expect(page.getByText('Keine Internetverbindung – Änderungen können gerade nicht gespeichert werden.')).toBeVisible();

  await von.fill('07:45');
  await von.blur();
  await expect(page.getByText('1 Zeit wegen der Verbindung nicht gespeichert')).toBeVisible();

  await context.setOffline(false);
  await expect(page.getByText('Keine Internetverbindung – Änderungen')).toHaveCount(0);
  await expect.poll(dbClockIn).toBe('07:45:00');
  await expect(page.getByText('wegen der Verbindung nicht gespeichert')).toHaveCount(0);
});

test('a load that fails once is retried without the user noticing', async ({ page }) => {
  let failed = 0;
  await page.route('**/api/v1/admin/employees/*/hours?month=*', (r) => {
    if (failed === 0) {
      failed++;
      return r.abort('internetdisconnected');
    }
    return r.continue();
  });
  await openMonth(page);
  expect(failed).toBe(1);
  await expect(page.getByRole('alert')).toHaveCount(0);
  await expect(row(page).getByLabel('Von')).toHaveValue('07:45');
});

test('a load that keeps failing shows an inline error with "Erneut laden"', async ({ page }) => {
  await openMonth(page);
  await page.route('**/api/v1/admin/employees/*/hours?month=*', (r) => r.abort('internetdisconnected'));
  await page.getByRole('button', { name: 'Vormonat' }).click();
  await expect(page.getByText('Daten konnten nicht geladen werden')).toBeVisible({ timeout: 15_000 });
  // Not last month's numbers under the new month's label.
  await expect(row(page)).toHaveCount(0);

  await page.unroute('**/api/v1/admin/employees/*/hours?month=*');
  await page.getByRole('button', { name: 'Nächster Monat' }).click();
  await expect(row(page)).toBeVisible();
  await expect(page.getByText('Daten konnten nicht geladen werden')).toHaveCount(0);
});

test('‹ / › step months; only complete months are requested, each once', async ({ page }) => {
  await openMonth(page);
  const months: string[] = [];
  page.on('request', (req) => {
    const m = /hours\?month=([^&]*)/.exec(req.url());
    if (m) months.push(m[1]);
  });
  await page.getByRole('button', { name: 'Vormonat' }).click();
  await page.waitForLoadState('networkidle');
  await page.getByRole('button', { name: 'Nächster Monat' }).click();
  await expect(row(page)).toBeVisible();
  await page.waitForLoadState('networkidle');

  const [y, m] = MONTH.split('-').map(Number);
  const prev = new Date(y, m - 2, 1);
  const prevMonth = `${prev.getFullYear()}-${String(prev.getMonth() + 1).padStart(2, '0')}`;
  expect(months).toEqual([prevMonth, MONTH]);
});
