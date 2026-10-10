import { test, expect, request as playwrightRequest } from '@playwright/test';
import type { APIRequestContext, Page } from '@playwright/test';
import {
  API_BASE,
  adminToken,
  createCustomer,
  createEmployee,
  createInquiry,
  createTerminWithEmployee,
  patchInquiry,
  assignInquiryEmployee,
  deleteInquiry,
  deleteEmployee,
  injectAdminAuth,
} from './worker-helpers';

/**
 * CUSTOMER PHONE EVERYWHERE — the primary contact must be one tap away.
 *
 * Every place a job shows up (calendar week/day views and side panel, Termine,
 * the employee's Einsätze table) renders the customer's phone as a `tel:` link.
 * Those links sit inside clickable cards/rows, so a tap must dial, not open the
 * detail panel underneath.
 */
test.use({ channel: 'chrome', video: 'off', viewport: { width: 1440, height: 1000 }, timezoneId: 'Europe/Berlin' });
test.describe.configure({ mode: 'serial', retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';
const TODAY = new Date().toISOString().slice(0, 10);
// createCustomer() seeds phone "0151 0000000".
const TEL = 'a[href="tel:01510000000"]';
const TERMIN_TITLE = `Telefon-Termin ${Date.now()}`;

let api: APIRequestContext;
let token: string;
let customerId: string;
let employeeId: string;
let inquiryId: string;
let terminId: string;

test.beforeAll(async () => {
  api = await playwrightRequest.newContext();
  token = await adminToken(api);
  customerId = (await createCustomer(api, token)).id;
  employeeId = (await createEmployee(api, token)).id;
  inquiryId = (await createInquiry(api, token, customerId, TODAY)).id;
  await patchInquiry(api, token, inquiryId, { status: 'scheduled' });
  await assignInquiryEmployee(api, token, inquiryId, employeeId, 'phone links e2e');
  terminId = await createTerminWithEmployee(api, token, employeeId, {
    title: TERMIN_TITLE,
    location: 'Hildesheim',
    scheduled_date: TODAY,
    start_time: '13:00:00',
  });
  const res = await api.patch(`${API_BASE}/api/v1/admin/calendar-items/${terminId}`, {
    headers: { Authorization: `Bearer ${token}` },
    data: { customer_id: customerId },
  });
  expect(res.ok()).toBeTruthy();
});

test.afterAll(async () => {
  await api.delete(`${API_BASE}/api/v1/admin/calendar-items/${terminId}`, {
    headers: { Authorization: `Bearer ${token}` },
  }).catch(() => {});
  await deleteInquiry(api, token, inquiryId);
  await deleteEmployee(api, token, employeeId);
  await api.dispose();
});

/** Swallow the tel: navigation so a test click doesn't leave the page. */
async function blockDialing(page: Page) {
  await page.evaluate(() =>
    document.addEventListener(
      'click',
      (e) => {
        if ((e.target as Element).closest('a[href^="tel:"]')) e.preventDefault();
      },
      true
    )
  );
}

test('week view: job and Termin cards show a tel: link that does not open the panel', async ({ page }) => {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/calendar`);
  await page.getByRole('button', { name: 'Woche' }).click();
  await page.waitForLoadState('networkidle');

  const terminCard = page.getByRole('button', { name: new RegExp(TERMIN_TITLE) }).last();
  await expect(terminCard.locator(TEL)).toBeVisible();

  await blockDialing(page);
  await terminCard.locator(TEL).click();
  await page.waitForTimeout(300);
  await expect(page.getByRole('heading', { name: TERMIN_TITLE })).toHaveCount(0);

  // The card itself still opens the Termin panel, now with the customer + phone.
  await terminCard.getByText(TERMIN_TITLE).click();
  const panel = page.getByRole('complementary', { name: 'Details' });
  await expect(panel.getByText('Repro Kunde')).toBeVisible();
  await expect(panel.locator(TEL).first()).toBeVisible();
});

test('day view shows the phone on the job', async ({ page }) => {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/calendar`);
  await page.getByRole('button', { name: 'Tag', exact: true }).click();
  await page.waitForLoadState('networkidle');
  const terminEntry = page.getByRole('button', { name: new RegExp(TERMIN_TITLE) }).last();
  await expect(terminEntry.locator(TEL)).toBeVisible();
});

test('employee Einsätze table shows the phone for job and Termin', async ({ page }) => {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/employees/${employeeId}`);
  await page.getByRole('button', { name: 'Monat' }).click();
  await page.waitForLoadState('networkidle');
  await expect(page.locator('tr').filter({ hasText: TERMIN_TITLE }).locator(TEL)).toBeVisible();
  await expect(page.locator('tr').filter({ hasText: 'Repro Kunde' }).filter({ hasNotText: TERMIN_TITLE }).locator(TEL).first()).toBeVisible();

  // Tapping the number must not navigate to the job.
  await blockDialing(page);
  await page.locator('tr').filter({ hasText: TERMIN_TITLE }).locator(TEL).click();
  await expect(page).toHaveURL(new RegExp(`/admin/employees/${employeeId}`));
});

test('Termin detail page shows the phone', async ({ page }) => {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/calendar-items/${terminId}`);
  await page.waitForLoadState('networkidle');
  await expect(page.locator(TEL)).toBeVisible();
});
