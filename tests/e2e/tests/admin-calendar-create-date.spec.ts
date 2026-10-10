import { test, expect, request as playwrightRequest } from '@playwright/test';
import type { APIRequestContext } from '@playwright/test';
import { API_BASE, adminToken, injectAdminAuth } from './worker-helpers';

/**
 * CALENDAR QUICK-CREATE DATE — on a phone the + button is the only way to create
 * an Anfrage/Termin, so the form must let you pick the date (it used to take
 * whatever day was in view, with no way to change it). The day view opens on
 * today and pre-fills the day being looked at.
 */
test.use({ channel: 'chrome', video: 'off', viewport: { width: 390, height: 844 }, timezoneId: 'Europe/Berlin' });
test.describe.configure({ mode: 'serial', retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';
const pad = (n: number) => String(n).padStart(2, '0');
const iso = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
const TODAY = iso(new Date());
const TOMORROW = iso(new Date(Date.now() + 86_400_000));
const PICKED = iso(new Date(Date.now() + 9 * 86_400_000));
const TITLE = `Datum-Termin ${Date.now()}`;

let api: APIRequestContext;
let token: string;

test.beforeAll(async () => {
  api = await playwrightRequest.newContext();
  token = await adminToken(api);
});

test.afterAll(async () => {
  const res = await api.get(`${API_BASE}/api/v1/admin/calendar-items?month=${PICKED.slice(0, 7)}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  if (res.ok()) {
    const body = await res.json();
    const items = (Array.isArray(body) ? body : body.items ?? []) as { id: string; title: string }[];
    for (const it of items.filter((i) => i.title === TITLE)) {
      await api.delete(`${API_BASE}/api/v1/admin/calendar-items/${it.id}`, { headers: { Authorization: `Bearer ${token}` } });
    }
  }
  await api.dispose();
});

async function openFab(page: import('@playwright/test').Page, label: string) {
  await page.getByRole('button', { name: 'Eintrag erstellen' }).click();
  await page.getByRole('button', { name: label }).click();
}

test('+ → Termin has an editable date and saves on the picked day', async ({ page }) => {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/calendar`);
  await page.waitForLoadState('networkidle');

  await openFab(page, 'Termin erstellen');
  const date = page.locator('#qt-date');
  await expect(date).toHaveValue(TODAY);
  await date.fill(PICKED);
  await page.locator('#qt-title').fill(TITLE);
  await page.getByRole('button', { name: 'Termin erstellen' }).last().click();
  await expect(page.locator('#qt-title')).toHaveCount(0);

  const res = await api.get(`${API_BASE}/api/v1/admin/calendar-items?month=${PICKED.slice(0, 7)}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  expect(res.ok()).toBeTruthy();
  const body = await res.json();
  const items = (Array.isArray(body) ? body : body.items ?? []) as { title: string; scheduled_date: string }[];
  expect(items.find((i) => i.title === TITLE)?.scheduled_date).toBe(PICKED);
});

test('+ → Anfrage has an editable date', async ({ page }) => {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/calendar`);
  await page.waitForLoadState('networkidle');
  await openFab(page, 'Anfrage erstellen');
  await expect(page.locator('#qi-date')).toHaveValue(TODAY);
  await page.locator('#qi-date').fill(PICKED);
  await expect(page.locator('#qi-date')).toHaveValue(PICKED);
});

test('day view opens on today and pre-fills the shown day', async ({ page }) => {
  await injectAdminAuth(page, token);
  await page.goto(`${FRONT}/admin/calendar`);
  await page.waitForLoadState('networkidle');

  // Browse away in month view first — switching to Tag must still land on today.
  await page.getByRole('button', { name: 'Weiter' }).click();
  await page.getByRole('button', { name: 'Tag', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Heute' })).toHaveCount(0);

  await page.getByRole('button', { name: 'Weiter' }).click();
  await expect(page.getByRole('button', { name: 'Heute' })).toBeVisible();
  await openFab(page, 'Termin erstellen');
  await expect(page.locator('#qt-date')).toHaveValue(TOMORROW);
});
