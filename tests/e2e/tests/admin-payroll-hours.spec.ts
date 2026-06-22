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
 * PAYROLL HOURS / STUNDENKONTO — admin month-end editing.
 *
 * Background: some movers are on a fixed hour base but work far more. At
 * month-end Alex must REDUCE the paid-out hours on the Stundenzettel without
 * destroying the recorded worked truth, and see the surplus ("Stundenkonto").
 *
 * This exercises the full override layer end-to-end against a **multi-day
 * appointment** (one inquiry spanning four consecutive days — historically the
 * fragile case), then the **destructive "Stundenkonto säubern"** that bakes the
 * overrides into the recorded data and discards the override layer.
 *
 *   seed:   4-day move, 08:00–17:00, 60-min break each → 8 h/day = 32 h worked
 *   edit:   deactivate day 1            → paid 24 h, account +8
 *           adjust day 2 to 10:00–16:00 → paid 21 h, account +11
 *   save:   persists; recorded clock times untouched
 *   säubern: day 1 assignment removed, day 2 recorded times overwritten,
 *            override layer discarded → account back to 0
 *
 * The whole file runs serially: the cleanup test consumes the adjustments the
 * edit test saved.
 */
test.use({ channel: 'chrome', video: 'off', viewport: { width: 1366, height: 900 }, timezoneId: 'Europe/Berlin' });
test.describe.configure({ mode: 'serial', retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';

// Four consecutive days inside the current month (the admin page defaults its
// month view to `new Date()`'s month). Days 01–04 are always in-month.
const MONTH = new Date().toISOString().slice(0, 7); // YYYY-MM
const DAYS = ['01', '02', '03', '04'].map((d) => `${MONTH}-${d}`);
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

  // A 4-day move: create on day 1, stretch end_date to day 4, THEN assign — the
  // assignment fans out one inquiry_employees row per day in the range.
  inquiryId = (await createInquiry(api, token, customerId, DAYS[0])).id;
  await patchInquiry(api, token, inquiryId, { status: 'scheduled', end_date: DAYS[3] });
  await assignInquiryEmployee(api, token, inquiryId, employeeId, 'Stundenkonto e2e');

  // Record 08:00–17:00 with a 60-min break on every day → 8 h worked each.
  for (const day of DAYS) {
    await setAssignmentHours(api, token, inquiryId, employeeId, day, {
      clock_in: '08:00:00',
      clock_out: '17:00:00',
      break_minutes: 60,
    });
  }
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

/** The summary value next to a given label ("Gearbeitet" / "Bezahlt" / "Stundenkonto"). */
function summaryValue(page: import('@playwright/test').Page, label: string) {
  return page.locator('.hours-row').filter({ hasText: label }).locator('.hours-value');
}

test('deactivate + adjust a multi-day move recomputes the Stundenkonto live and persists', async ({ page }) => {
  await openMonth(page);

  // All four days carry Von/Bis, so the gate is open and edit mode is allowed.
  await expect(summaryValue(page, 'Gearbeitet')).toHaveText('32.0 h');
  await expect(summaryValue(page, 'Bezahlt')).toHaveText('32.0 h');
  const editBtn = page.getByRole('button', { name: 'Bearbeiten' });
  await expect(editBtn).toBeEnabled();
  await editBtn.click();

  // Row helper: the table row carrying a given day's date.
  const row = (day: string) => page.locator('tr', { hasText: de(day) });

  // --- deactivate day 1 (uncheck "Aktiv") → paid 24 h, account +8 ---
  await row(DAYS[0]).locator('input[type="checkbox"]').uncheck();
  await expect(summaryValue(page, 'Bezahlt')).toHaveText('24.0 h');
  await expect(summaryValue(page, 'Stundenkonto')).toHaveText('+8.0 h');

  // --- adjust day 2 paid times to 10:00–16:00 (still 60-min break) → 5 h ---
  const r2 = row(DAYS[1]);
  await r2.locator('input.time-input').first().fill('10:00');
  await r2.locator('input.time-input').nth(1).fill('16:00');
  // blur so the derived paid-hours recompute fires
  await r2.locator('input.break-input').click();

  // worked 32, paid = 0 + 5 + 8 + 8 = 21 → account +11, live, no save yet
  await expect(summaryValue(page, 'Bezahlt')).toHaveText('21.0 h');
  await expect(summaryValue(page, 'Stundenkonto')).toHaveText('+11.0 h');
  await expect(summaryValue(page, 'Gearbeitet')).toHaveText('32.0 h');

  // --- save & leave edit mode ---
  await page.getByRole('button', { name: 'Speichern & Beenden' }).click();
  await expect(page.getByRole('button', { name: 'Bearbeiten' })).toBeVisible();

  // persisted totals after refetch
  await expect(summaryValue(page, 'Bezahlt')).toHaveText('21.0 h');
  await expect(summaryValue(page, 'Stundenkonto')).toHaveText('+11.0 h');
  // worked truth untouched — still 32 h recorded
  await expect(summaryValue(page, 'Gearbeitet')).toHaveText('32.0 h');

  // a hard reload proves it came from the server, not local state
  await page.reload();
  await page.waitForLoadState('networkidle');
  await page.getByRole('button', { name: 'Monat' }).click();
  await page.waitForLoadState('networkidle');
  await expect(summaryValue(page, 'Stundenkonto')).toHaveText('+11.0 h');

  // and the recorded clock times in the override table are unchanged — the API
  // still reports 8 h worked for the adjusted day (paid 5, worked 8).
  const summary = await (await api.get(
    `${API_BASE}/api/v1/admin/employees/${employeeId}/hours?month=${MONTH}`,
    { headers: { Authorization: `Bearer ${token}` } }
  )).json();
  const day2 = summary.assignments.find((a: { booking_date: string }) => a.booking_date === DAYS[1]);
  expect(day2.worked_hours).toBeCloseTo(8, 1);
  expect(day2.paid_hours).toBeCloseTo(5, 1);
});

test('Stundenkonto säubern bakes overrides into recorded data and clears the layer', async ({ page }) => {
  await openMonth(page);

  // The saved overrides surface the destructive button.
  const cleanupBtn = page.getByRole('button', { name: 'Stundenkonto säubern' });
  await expect(cleanupBtn).toBeVisible();
  await cleanupBtn.click();

  // confirm the irreversible action
  await page.getByRole('button', { name: 'Endgültig säubern' }).click();
  await expect(page.getByText('Stundenkonto gesäubert')).toBeVisible({ timeout: 10_000 });

  // day 1 (deactivated) is gone — the employee was removed from that day
  await expect(page.locator('tr', { hasText: de(DAYS[0]) })).toHaveCount(0);
  // the three surviving days are still there
  await expect(page.locator('tr', { hasText: de(DAYS[1]) })).toHaveCount(1);
  await expect(page.locator('tr', { hasText: de(DAYS[3]) })).toHaveCount(1);

  // day 2 recorded times were overwritten to the adjusted 10:00 start
  await expect(
    page.locator('tr', { hasText: de(DAYS[1]) }).locator('input.time-input').first()
  ).toHaveValue('10:00');

  // override layer discarded: worked == paid (5 + 8 + 8 = 21), account back to 0
  await expect(summaryValue(page, 'Gearbeitet')).toHaveText('21.0 h');
  await expect(summaryValue(page, 'Bezahlt')).toHaveText('21.0 h');
  await expect(summaryValue(page, 'Stundenkonto')).toHaveText('+0.0 h');
  // and the destructive button is gone — nothing left to säubern
  await expect(page.getByRole('button', { name: 'Stundenkonto säubern' })).toHaveCount(0);

  // server confirms the cleanup: 3 days, none deactivated, no paid overrides
  const summary = await (await api.get(
    `${API_BASE}/api/v1/admin/employees/${employeeId}/hours?month=${MONTH}`,
    { headers: { Authorization: `Bearer ${token}` } }
  )).json();
  expect(summary.assignments).toHaveLength(3);
  for (const a of summary.assignments) {
    expect(a.deactivated).toBe(false);
    expect(a.paid_clock_in ?? null).toBeNull();
  }
});

test('the edit gate stays closed until every day has Von/Bis times', async ({ page, browser }) => {
  // A fresh employee with a single move whose hours were never recorded — the
  // "Bearbeiten" button must stay disabled (you cannot reduce hours you have
  // not yet recorded).
  const cust = (await createCustomer(api, token)).id;
  const emp = (await createEmployee(api, token)).id;
  const inq = (await createInquiry(api, token, cust, DAYS[0])).id;
  await patchInquiry(api, token, inq, { status: 'scheduled' });
  await assignInquiryEmployee(api, token, inq, emp, 'gate e2e');

  try {
    const ctx = await browser.newContext({ viewport: { width: 1366, height: 900 }, timezoneId: 'Europe/Berlin' });
    const p = await ctx.newPage();
    await injectAdminAuth(p, token);
    await p.goto(`${FRONT}/admin/employees/${emp}`);
    await p.waitForLoadState('networkidle');
    await p.getByRole('button', { name: 'Monat' }).click();
    await p.waitForLoadState('networkidle');

    await expect(p.getByRole('button', { name: 'Bearbeiten' })).toBeDisabled();
    await ctx.close();
  } finally {
    await deleteInquiry(api, token, inq);
    await deleteEmployee(api, token, emp);
    await api.post(`${API_BASE}/api/v1/admin/customers/${cust}/delete`, {
      headers: { Authorization: `Bearer ${token}` },
    }).catch(() => {});
  }
});
