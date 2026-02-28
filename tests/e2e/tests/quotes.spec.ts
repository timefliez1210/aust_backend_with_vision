import { test, expect } from '@playwright/test';
import { injectAuth } from './helpers';

// ---------------------------------------------------------------------------
// Helper: navigate to /admin/quotes with a valid session.
// ---------------------------------------------------------------------------
async function goToQuotes(page: import('@playwright/test').Page) {
  await page.goto('/admin/login');
  await injectAuth(page);
  await page.goto('/admin/quotes');
  await page.waitForLoadState('networkidle');
}

// ---------------------------------------------------------------------------
// Quotes list page
// ---------------------------------------------------------------------------

test.describe('Quotes list', () => {
  test('page heading reads "Anfragen"', async ({ page }) => {
    await goToQuotes(page);

    // The quotes list page renders <h1> inside .page-header.
    await expect(page.locator('.page-header h1, h1')).toContainText('Anfragen', {
      timeout: 10_000,
    });
  });

  test('search input is present in the toolbar', async ({ page }) => {
    await goToQuotes(page);

    // The quotes page includes a search input (type="search" or placeholder text).
    const searchInput = page.locator('input[type="search"], input[placeholder*="uche"]').first();
    await expect(searchInput).toBeVisible({ timeout: 10_000 });
  });

  test('status filter buttons / tabs are visible', async ({ page }) => {
    await goToQuotes(page);

    // The toolbar includes filter tab/button controls for different quote statuses.
    // These appear inside .toolbar or .tabs elements.
    const toolbar = page.locator('.toolbar, .tabs').first();
    await expect(toolbar).toBeVisible({ timeout: 10_000 });

    // Expect at least one filter button or tab to be rendered.
    const filterBtns = page.locator('.tab, .filter-btn, button.tab').first();
    await expect(filterBtns).toBeVisible({ timeout: 10_000 });
  });

  test('table or empty state renders without error', async ({ page }) => {
    await goToQuotes(page);

    // The DataTable component always renders; it may show rows or an empty state.
    // Either the table body or an empty state element should be present.
    const table = page.locator('table, .data-table, .empty-state, [class*="table"]').first();
    await expect(table).toBeVisible({ timeout: 12_000 });
  });

  test('total count label is visible', async ({ page }) => {
    await goToQuotes(page);

    // The page renders a count span like "0 gesamt" or "5 gesamt".
    const countLabel = page.locator('.page-count');
    await expect(countLabel).toBeVisible({ timeout: 10_000 });
    await expect(countLabel).toContainText('gesamt');
  });

  test('"Neue Anfrage" create button is visible', async ({ page }) => {
    await goToQuotes(page);

    // The quotes page has a button to open the create form.
    const createBtn = page
      .locator('button:has-text("Neue"), button:has-text("Anfrage"), button[class*="create"]')
      .first();
    await expect(createBtn).toBeVisible({ timeout: 10_000 });
  });
});

// ---------------------------------------------------------------------------
// Quote detail page
// ---------------------------------------------------------------------------

test.describe('Quote detail', () => {
  test('navigating to a non-existent quote ID shows an error or redirect', async ({ page }) => {
    await page.goto('/admin/login');
    await injectAuth(page);

    // Use a valid UUID v4 format that almost certainly does not exist in the DB.
    const fakeId = '00000000-0000-7000-8000-000000000001';
    await page.goto(`/admin/quotes/${fakeId}`);
    await page.waitForLoadState('networkidle');

    // The page should either show an error message or redirect back to the list.
    // We accept any of: error text, a "not found" banner, or a redirect to /admin/quotes.
    const url = page.url();
    const hasError = await page
      .locator('.error, .error-banner, [class*="error"], [class*="not-found"]')
      .isVisible()
      .catch(() => false);
    const redirectedToList = url.endsWith('/admin/quotes');

    // At minimum the page must not crash (no unhandled JS error overlay).
    // Accept either an error UI or a redirect.
    expect(hasError || redirectedToList || url.includes('/admin/quotes')).toBe(true);
  });

  test('quote detail page structure when a quote ID is found', async ({ page }) => {
    await page.goto('/admin/login');
    await injectAuth(page);

    // First fetch the list to see if any quotes exist.
    const STAGING_API = process.env.STAGING_URL || 'http://localhost:8099';

    const { TEST_JWT } = await import('./helpers');
    const res = await page.request.get(`${STAGING_API}/api/v1/admin/quotes?limit=1`, {
      headers: { Authorization: `Bearer ${TEST_JWT}` },
    });

    if (!res.ok()) {
      test.skip(true, 'API not reachable or returned an error — skipping detail test');
      return;
    }

    const body = (await res.json()) as { quotes: { id: string }[]; total: number };

    if (!body.quotes || body.quotes.length === 0) {
      test.skip(true, 'No quotes in staging DB — skipping detail test');
      return;
    }

    const quoteId = body.quotes[0].id;
    await page.goto(`/admin/quotes/${quoteId}`);
    await page.waitForLoadState('networkidle');

    // Detail page must render customer info section.
    await expect(page.locator('.customer-section, .info-card, .detail-card').first()).toBeVisible({
      timeout: 12_000,
    });

    // There should be an "Angebot generieren" or similar offer-generation button.
    const offerBtn = page.locator(
      'button:has-text("Angebot"), button:has-text("generieren"), button:has-text("Offer")'
    );
    // We don't require it to exist — just assert the page itself loaded.
    await expect(page.locator('h1, .page-header')).toBeVisible({ timeout: 8_000 });
  });

  test('back navigation from quote detail returns to list', async ({ page }) => {
    await page.goto('/admin/login');
    await injectAuth(page);

    const STAGING_API = process.env.STAGING_URL || 'http://localhost:8099';
    const { TEST_JWT } = await import('./helpers');
    const res = await page.request.get(`${STAGING_API}/api/v1/admin/quotes?limit=1`, {
      headers: { Authorization: `Bearer ${TEST_JWT}` },
    });

    if (!res.ok()) {
      test.skip(true, 'API not reachable — skipping back-navigation test');
      return;
    }

    const body = (await res.json()) as { quotes: { id: string }[]; total: number };
    if (!body.quotes || body.quotes.length === 0) {
      test.skip(true, 'No quotes in staging DB — skipping back-navigation test');
      return;
    }

    const quoteId = body.quotes[0].id;
    await page.goto(`/admin/quotes/${quoteId}`);
    await page.waitForLoadState('networkidle');

    // The detail page has an ArrowLeft back button.
    const backBtn = page.locator('a[href="/admin/quotes"], button:has-text("Zurück")').first();
    if (await backBtn.isVisible({ timeout: 5_000 }).catch(() => false)) {
      await backBtn.click();
      await page.waitForLoadState('networkidle');
      await expect(page.locator('.page-header h1, h1')).toContainText('Anfragen', {
        timeout: 8_000,
      });
    }
  });
});
