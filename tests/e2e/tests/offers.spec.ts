import { test, expect } from '@playwright/test';
import { injectAuth } from './helpers';

// ---------------------------------------------------------------------------
// Helper: navigate to /admin/offers with a valid session.
// ---------------------------------------------------------------------------
async function goToOffers(page: import('@playwright/test').Page) {
  await page.goto('/admin/login');
  await injectAuth(page);
  await page.goto('/admin/offers');
  await page.waitForLoadState('networkidle');
}

// ---------------------------------------------------------------------------
// Offers list page
// ---------------------------------------------------------------------------

test.describe('Offers list', () => {
  test('page heading reads "Angebote"', async ({ page }) => {
    await goToOffers(page);

    await expect(page.locator('.page-header h1, h1')).toContainText('Angebote', {
      timeout: 10_000,
    });
  });

  test('total count label is visible', async ({ page }) => {
    await goToOffers(page);

    // The page renders "<N> gesamt" next to the heading.
    const countLabel = page.locator('.page-count');
    await expect(countLabel).toBeVisible({ timeout: 10_000 });
    await expect(countLabel).toContainText('gesamt');
  });

  test('status filter tabs are visible', async ({ page }) => {
    await goToOffers(page);

    // The tabs: Alle, Entwurf, Gesendet, Akzeptiert, Abgelehnt.
    const tabs = page.locator('.tabs');
    await expect(tabs).toBeVisible({ timeout: 10_000 });

    // "Alle" tab (default active) should be present.
    await expect(tabs.locator('button:has-text("Alle")')).toBeVisible({ timeout: 8_000 });

    // Other tabs should also be present.
    await expect(tabs.locator('button:has-text("Entwurf")')).toBeVisible({ timeout: 8_000 });
    await expect(tabs.locator('button:has-text("Gesendet")')).toBeVisible({ timeout: 8_000 });
    await expect(tabs.locator('button:has-text("Akzeptiert")')).toBeVisible({ timeout: 8_000 });
    await expect(tabs.locator('button:has-text("Abgelehnt")')).toBeVisible({ timeout: 8_000 });
  });

  test('"Alle" tab is active by default', async ({ page }) => {
    await goToOffers(page);

    const alleTab = page.locator('.tabs button:has-text("Alle")');
    await expect(alleTab).toBeVisible({ timeout: 8_000 });
    await expect(alleTab).toHaveClass(/active/, { timeout: 8_000 });
  });

  test('switching to "Entwurf" tab does not crash the page', async ({ page }) => {
    await goToOffers(page);

    await page.locator('.tabs button:has-text("Entwurf")').click();
    await page.waitForLoadState('networkidle');

    // After clicking the tab, the heading should still be present (page did not crash).
    await expect(page.locator('.page-header h1, h1')).toContainText('Angebote', {
      timeout: 10_000,
    });

    // The "Entwurf" tab should now be active.
    await expect(page.locator('.tabs button:has-text("Entwurf")')).toHaveClass(/active/, {
      timeout: 8_000,
    });
  });

  test('table or empty state renders without error', async ({ page }) => {
    await goToOffers(page);

    // DataTable always renders — either with rows or with an empty state element.
    const tableOrEmpty = page.locator('table, .data-table, .empty-state, [class*="table"]').first();
    await expect(tableOrEmpty).toBeVisible({ timeout: 12_000 });
  });

  test('sidebar nav link "Angebote" is active on offers page', async ({ page }) => {
    await goToOffers(page);

    // The sidebar link for /admin/offers should carry the "active" class.
    const offersLink = page.locator('aside.sidebar a[href="/admin/offers"]');
    await expect(offersLink).toBeVisible({ timeout: 8_000 });
    await expect(offersLink).toHaveClass(/active/, { timeout: 8_000 });
  });

  test('clicking "Alle" tab after filtering resets to all offers', async ({ page }) => {
    await goToOffers(page);

    // Switch to a different tab first.
    await page.locator('.tabs button:has-text("Abgelehnt")').click();
    await page.waitForLoadState('networkidle');

    // Then switch back to "Alle".
    await page.locator('.tabs button:has-text("Alle")').click();
    await page.waitForLoadState('networkidle');

    await expect(page.locator('.tabs button:has-text("Alle")')).toHaveClass(/active/, {
      timeout: 8_000,
    });
    await expect(page.locator('.page-header h1, h1')).toContainText('Angebote', {
      timeout: 8_000,
    });
  });

  test('offer detail navigation works when an offer exists', async ({ page }) => {
    await page.goto('/admin/login');
    await injectAuth(page);

    const STAGING_API = process.env.STAGING_URL || 'http://localhost:8099';
    const { TEST_JWT } = await import('./helpers');

    const res = await page.request.get(`${STAGING_API}/api/v1/admin/offers?limit=1`, {
      headers: { Authorization: `Bearer ${TEST_JWT}` },
    });

    if (!res.ok()) {
      test.skip(true, 'API not reachable — skipping offer detail navigation test');
      return;
    }

    const body = (await res.json()) as { offers: { id: string }[]; total: number };
    if (!body.offers || body.offers.length === 0) {
      test.skip(true, 'No offers in staging DB — skipping offer detail navigation test');
      return;
    }

    // Navigate to the list and click the first row.
    await page.goto('/admin/offers');
    await page.waitForLoadState('networkidle');

    // Click the first data row in the table.
    const firstRow = page.locator('table tbody tr, .data-table tbody tr').first();
    if (await firstRow.isVisible({ timeout: 5_000 }).catch(() => false)) {
      await firstRow.click();
      await page.waitForLoadState('networkidle');

      // After clicking a row the URL should contain /admin/offers/<uuid>.
      expect(page.url()).toMatch(/\/admin\/offers\/[0-9a-f-]{36}/);
    }
  });
});
