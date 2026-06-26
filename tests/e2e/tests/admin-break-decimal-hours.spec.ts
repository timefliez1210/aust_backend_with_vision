import { test, expect, request as playwrightRequest } from '@playwright/test';
import type { APIRequestContext } from '@playwright/test';
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
 * DECIMAL-HOURS BREAK ENTRY — admin types breaks as decimal hours, DB stays minutes.
 *
 * Background: Alex wants to log breaks as decimal hours ("0.25") instead of raw
 * minutes. The break is still PERSISTED as integer minutes (the `break_minutes`
 * column is unchanged); the frontend converts decimal-hours ⇄ minutes at the
 * input edge. Minutes is the canonical snap, so messy thirds land on a clean
 * value both ways:
 *
 *   type 0.25 → store 15 min → display "0.25"
 *   type 0.33 → store 20 min → display "0.33"   (0.33×60 = 19.8 → 20; 20/60 → "0.33")
 *
 * This drives the real admin employee-hours table (the worked-hours break input,
 * which blur-saves a PATCH), asserts the persisted minutes via the API, and
 * proves the value survives a hard reload as the same decimal.
 */
test.use({ channel: 'chrome', video: 'off', viewport: { width: 1366, height: 900 }, timezoneId: 'Europe/Berlin' });
test.describe.configure({ mode: 'serial', retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';

// A single day inside the current month (the employee page defaults to this month).
const MONTH = new Date().toISOString().slice(0, 7); // YYYY-MM
const DAY = `${MONTH}-05`;
/** ISO `YYYY-MM-DD` → German `DD.MM.YYYY` as rendered by `formatDate`. */
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
  await assignInquiryEmployee(api, token, inquiryId, employeeId, 'break decimal e2e');

  // Seed 08:00–17:00 with a 60-minute break → the field starts at "1" (= 1 h).
  await setAssignmentHours(api, token, inquiryId, employeeId, DAY, {
    clock_in: '08:00:00',
    clock_out: '17:00:00',
    break_minutes: 60,
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

/** Open the employee detail page in the month view, authenticated as admin. */
async function openMonth(page: import('@playwright/test').Page) {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/employees/${employeeId}`);
  await page.waitForLoadState('networkidle');
  await page.getByRole('button', { name: 'Monat' }).click();
  await page.waitForLoadState('networkidle');
}

/** The seeded day's assignment row from the hours summary API (DB source of truth). */
async function dbAssignment(): Promise<{ break_minutes: number; worked_hours: number; actual_hours: number | null }> {
  const summary = await (await api.get(
    `${API_BASE}/api/v1/admin/employees/${employeeId}/hours?month=${MONTH}`,
    { headers: { Authorization: `Bearer ${token}` } }
  )).json();
  return summary.assignments.find((x: { booking_date: string }) => x.booking_date === DAY);
}

/** The persisted break minutes for our seeded day, read straight from the API. */
async function dbBreakMinutes(): Promise<number> {
  return (await dbAssignment()).break_minutes;
}

/** The summary value next to a label ("Gearbeitet" / "Bezahlt" / "Stundenkonto"). */
function summaryValue(page: import('@playwright/test').Page, label: string) {
  return page.locator('.hours-row').filter({ hasText: label }).locator('.hours-value');
}

/** The worked-hours break input on our seeded day's row. */
function breakInput(page: import('@playwright/test').Page) {
  return page.locator('tr', { hasText: de(DAY) }).locator('input.break-input');
}

test('typing 0.25 stores 15 minutes and round-trips as "0.25"', async ({ page }) => {
  await openMonth(page);

  // Seeded 60 min renders as decimal hours "1".
  await expect(breakInput(page)).toHaveValue('1');

  // Type a quarter-hour break and blur to fire the PATCH.
  await breakInput(page).fill('0.25');
  await breakInput(page).blur();

  // The field immediately reflects the canonical decimal …
  await expect(breakInput(page)).toHaveValue('0.25');
  // … and the DB stored exactly 15 minutes (PATCH is async → poll).
  await expect.poll(dbBreakMinutes).toBe(15);

  // A hard reload proves it came from the server, not local state.
  await page.reload();
  await page.waitForLoadState('networkidle');
  await page.getByRole('button', { name: 'Monat' }).click();
  await page.waitForLoadState('networkidle');
  await expect(breakInput(page)).toHaveValue('0.25');
});

test('typing 0.33 snaps to the closest clean value: 20 minutes, displayed as "0.33"', async ({ page }) => {
  await openMonth(page);

  // Carried over from the previous test.
  await expect(breakInput(page)).toHaveValue('0.25');

  // 0.33 h = 19.8 min → rounds to 20 (the closest clean minute value).
  await breakInput(page).fill('0.33');
  await breakInput(page).blur();

  await expect(breakInput(page)).toHaveValue('0.33');
  await expect.poll(dbBreakMinutes).toBe(20);

  // And 20 min must render back as "0.33", not drift to "0.3"/"0.34".
  await page.reload();
  await page.waitForLoadState('networkidle');
  await page.getByRole('button', { name: 'Monat' }).click();
  await page.waitForLoadState('networkidle');
  await expect(breakInput(page)).toHaveValue('0.33');
});

test('a German decimal comma ("0,75") is accepted and stored as 45 minutes', async ({ page }) => {
  await openMonth(page);

  await breakInput(page).fill('0,75');
  await breakInput(page).blur();

  // Comma normalizes to a dot on the way in; canonical display uses a dot.
  await expect(breakInput(page)).toHaveValue('0.75');
  await expect.poll(dbBreakMinutes).toBe(45);
});

test('the calculated hour sheet recomputes worked hours from a decimal break edit', async ({ page }) => {
  // No regression in the derived hours: worked = gross window − break. Our day is
  // 08:00–17:00 (9 h gross). Editing the break through the decimal input must flow
  // through to the server-computed worked/actual hours AND the rendered summary.
  await openMonth(page);

  // 0.5 h break (30 min) → worked = 9 − 0.5 = 8.5 h.
  await breakInput(page).fill('0.5');
  await breakInput(page).blur();
  await expect(breakInput(page)).toHaveValue('0.5');
  await expect.poll(dbBreakMinutes).toBe(30);
  await expect.poll(async () => (await dbAssignment()).worked_hours).toBeCloseTo(8.5, 2);
  await expect(summaryValue(page, 'Gearbeitet')).toHaveText('8.5 h');

  // Change to 0.25 h (15 min) → worked = 9 − 0.25 = 8.75 h; the sheet tracks it.
  await breakInput(page).fill('0.25');
  await breakInput(page).blur();
  await expect.poll(dbBreakMinutes).toBe(15);
  await expect.poll(async () => (await dbAssignment()).worked_hours).toBeCloseTo(8.75, 2);
  await expect(summaryValue(page, 'Gearbeitet')).toHaveText('8.8 h');
});
