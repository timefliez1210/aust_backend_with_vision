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
  createTerminWithEmployee,
  assignTerminEmployee,
  setTerminHours,
  deleteInquiry,
  deleteEmployee,
  deleteTermin,
  injectAdminAuth,
} from './worker-helpers';

/**
 * STUNDENKONTO SÄUBERN — scope guarantees.
 *
 * "Stundenkonto säubern" is destructive and per-employee. This locks in that it
 * only ever touches the employee being cleaned up, on the exact days adjusted:
 *
 *   • a DEACTIVATED day removes *that* employee from the inquiry/Termin day —
 *     co-assigned colleagues stay on the job;
 *   • an ADJUSTED day overwrites *that* employee's recorded clock times —
 *     colleagues' recorded times are untouched;
 *   • both for multi-day inquiries and for single-day calendar Termine.
 *
 * Every job in these tests carries two crew members (the target + a witness);
 * after säubern we assert the witness is byte-for-byte unchanged.
 */
test.use({ channel: 'chrome', video: 'off', viewport: { width: 1366, height: 900 }, timezoneId: 'Europe/Berlin' });
test.describe.configure({ retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';
const MONTH = new Date().toISOString().slice(0, 7); // YYYY-MM
const DAYS = ['05', '06', '07', '08'].map((d) => `${MONTH}-${d}`);
const de = (iso: string) => { const [y, m, d] = iso.split('-'); return `${d}.${m}.${y}`; };

let api: APIRequestContext;
let token: string;

test.beforeAll(async () => {
  api = await playwrightRequest.newContext();
  token = await adminToken(api);
});
test.afterAll(async () => { await api.dispose(); });

/** Open an employee's hours page in the month view, authenticated as admin. */
async function openMonth(page: Page, employeeId: string) {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/employees/${employeeId}`);
  await page.waitForLoadState('networkidle');
  await page.getByRole('button', { name: 'Monat' }).click();
  await page.waitForLoadState('networkidle');
}

/** Fetch the raw hours summary for an employee (admin API). */
async function hoursSummary(employeeId: string): Promise<{
  assignments: Array<{ booking_date: string; worked_hours: number | null; clock_in: string | null; deactivated: boolean }>;
  calendar_items: Array<{ calendar_item_id: string; scheduled_date: string; worked_hours: number | null; clock_in: string | null; deactivated: boolean }>;
}> {
  const res = await api.get(`${API_BASE}/api/v1/admin/employees/${employeeId}/hours?month=${MONTH}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  return res.json();
}

test('multi-day inquiry: säubern removes only the target worker and rewrites only their times', async ({ page }) => {
  const customerId = (await createCustomer(api, token)).id;
  const target = (await createEmployee(api, token)).id;   // the worker being cleaned up
  const witness = (await createEmployee(api, token)).id;   // the colleague who must stay intact

  // 4-day move with both workers on every day, 08:00–17:00 / 60 min = 8 h/day.
  const inquiryId = (await createInquiry(api, token, customerId, DAYS[0])).id;
  await patchInquiry(api, token, inquiryId, { status: 'scheduled', end_date: DAYS[3] });
  await assignInquiryEmployee(api, token, inquiryId, target, 'target');
  await assignInquiryEmployee(api, token, inquiryId, witness, 'witness');
  for (const day of DAYS) {
    await setAssignmentHours(api, token, inquiryId, target, day, { clock_in: '08:00:00', clock_out: '17:00:00', break_minutes: 60 });
    await setAssignmentHours(api, token, inquiryId, witness, day, { clock_in: '08:00:00', clock_out: '17:00:00', break_minutes: 60 });
  }

  try {
    // --- as the target: deactivate day 1, adjust day 2, save, säubern ---
    await openMonth(page, target);
    await page.getByRole('button', { name: 'Bearbeiten' }).click();
    await page.locator('tr', { hasText: de(DAYS[0]) }).locator('input[type="checkbox"]').uncheck();
    const r2 = page.locator('tr', { hasText: de(DAYS[1]) });
    await r2.locator('input.time-input').first().fill('10:00');
    await r2.locator('input.time-input').nth(1).fill('16:00');
    await r2.locator('input.break-input').click();
    await page.getByRole('button', { name: 'Speichern & Beenden' }).click();
    await expect(page.getByRole('button', { name: 'Bearbeiten' })).toBeVisible();

    await page.getByRole('button', { name: 'Stundenkonto säubern' }).click();
    await page.getByRole('button', { name: 'Endgültig säubern' }).click();
    await expect(page.getByText('Stundenkonto gesäubert')).toBeVisible({ timeout: 10_000 });

    // --- target: day 1 removed, day 2 recorded times overwritten to 10:00 ---
    const t = await hoursSummary(target);
    expect(t.assignments).toHaveLength(3);
    expect(t.assignments.some((a) => a.booking_date === DAYS[0])).toBe(false);
    const tDay2 = t.assignments.find((a) => a.booking_date === DAYS[1])!;
    expect(tDay2.clock_in).toMatch(/^10:00/);
    expect(tDay2.worked_hours).toBeCloseTo(5, 1);

    // --- witness: still on all 4 days, every recorded time untouched (8 h) ---
    const w = await hoursSummary(witness);
    expect(w.assignments).toHaveLength(4);
    expect(w.assignments.some((a) => a.booking_date === DAYS[0])).toBe(true);
    for (const a of w.assignments) {
      expect(a.clock_in).toMatch(/^08:00/);
      expect(a.worked_hours).toBeCloseTo(8, 1);
      expect(a.deactivated).toBe(false);
    }

    // the witness's view persists across a reload (came from the server)
    await openMonth(page, witness);
    await expect(page.locator('tr', { hasText: de(DAYS[0]) })).toHaveCount(1);
    await expect(page.locator('tr', { hasText: de(DAYS[1]) }).locator('input.time-input').first()).toHaveValue('08:00');
  } finally {
    await deleteInquiry(api, token, inquiryId);
    await deleteEmployee(api, token, target);
    await deleteEmployee(api, token, witness);
    await api.post(`${API_BASE}/api/v1/admin/customers/${customerId}/delete`, { headers: { Authorization: `Bearer ${token}` } }).catch(() => {});
  }
});

test('single-day Termine: säubern removes the target from one and rewrites their time on another, colleague intact', async ({ page }) => {
  const target = (await createEmployee(api, token)).id;
  const witness = (await createEmployee(api, token)).id;
  const stamp = Date.now();
  const titleRemove = `Räumung ${stamp}`;   // empA deactivated here → removed
  const titleAdjust = `Lager ${stamp}`;     // empA time adjusted here → overwritten

  // Two single-day Termine, both crewed by target + witness, 08:00–17:00 / 60.
  const itemRemove = await createTerminWithEmployee(api, token, target, { title: titleRemove, location: 'Hildesheim', scheduled_date: DAYS[0] });
  const itemAdjust = await createTerminWithEmployee(api, token, target, { title: titleAdjust, location: 'Hannover', scheduled_date: DAYS[1] });
  await assignTerminEmployee(api, token, itemRemove, witness);
  await assignTerminEmployee(api, token, itemAdjust, witness);
  for (const [item, day] of [[itemRemove, DAYS[0]], [itemAdjust, DAYS[1]]] as const) {
    await setTerminHours(api, token, item, target, day, { clock_in: '08:00:00', clock_out: '17:00:00', break_minutes: 60 });
    await setTerminHours(api, token, item, witness, day, { clock_in: '08:00:00', clock_out: '17:00:00', break_minutes: 60 });
  }

  try {
    // --- as the target: deactivate the Räumung, adjust the Lager to 09:00–15:00 ---
    await openMonth(page, target);
    await page.getByRole('button', { name: 'Bearbeiten' }).click();
    await page.locator('tr', { hasText: titleRemove }).locator('input[type="checkbox"]').uncheck();
    const rAdj = page.locator('tr', { hasText: titleAdjust });
    await rAdj.locator('input.time-input').first().fill('09:00');
    await rAdj.locator('input.time-input').nth(1).fill('15:00');
    await rAdj.locator('input.break-input').click();
    await page.getByRole('button', { name: 'Speichern & Beenden' }).click();
    await page.getByRole('button', { name: 'Stundenkonto säubern' }).click();
    await page.getByRole('button', { name: 'Endgültig säubern' }).click();
    await expect(page.getByText('Stundenkonto gesäubert')).toBeVisible({ timeout: 10_000 });

    // --- target: removed from the Räumung, kept on the Lager with rewritten time ---
    const t = await hoursSummary(target);
    expect(t.calendar_items.some((c) => c.calendar_item_id === itemRemove)).toBe(false);
    const tAdj = t.calendar_items.find((c) => c.calendar_item_id === itemAdjust)!;
    expect(tAdj.clock_in).toMatch(/^09:00/);
    expect(tAdj.worked_hours).toBeCloseTo(5, 1);

    // --- witness: still on BOTH Termine, both recorded times untouched (8 h) ---
    const w = await hoursSummary(witness);
    const wRemove = w.calendar_items.find((c) => c.calendar_item_id === itemRemove)!;
    const wAdjust = w.calendar_items.find((c) => c.calendar_item_id === itemAdjust)!;
    expect(wRemove).toBeTruthy();
    expect(wRemove.clock_in).toMatch(/^08:00/);
    expect(wRemove.worked_hours).toBeCloseTo(8, 1);
    expect(wAdjust.clock_in).toMatch(/^08:00/);
    expect(wAdjust.worked_hours).toBeCloseTo(8, 1);
  } finally {
    await deleteTermin(api, token, itemRemove);
    await deleteTermin(api, token, itemAdjust);
    await deleteEmployee(api, token, target);
    await deleteEmployee(api, token, witness);
  }
});
