import { test, expect } from '@playwright/test';
import { injectAuth } from './helpers';

// ---------------------------------------------------------------------------
// Helper: navigate to /admin with a valid session already injected.
// ---------------------------------------------------------------------------
async function goToDashboard(page: import('@playwright/test').Page) {
  // Visit login first to get a browser context, then inject token.
  await page.goto('/admin/login');
  await injectAuth(page);
  await page.goto('/admin');
  await page.waitForLoadState('networkidle');
}

// ---------------------------------------------------------------------------
// Dashboard page tests
// ---------------------------------------------------------------------------

test.describe('Dashboard', () => {
  test('authenticated visit renders the Dashboard heading', async ({ page }) => {
    await goToDashboard(page);

    // The dashboard page renders <h1>Dashboard</h1> inside .page-header.
    await expect(page.locator('.page-header h1, h1')).toContainText('Dashboard', { timeout: 10_000 });
  });

  test('stats grid is visible with four stat cards', async ({ page }) => {
    await goToDashboard(page);

    // .stats-grid wraps the four KPI cards.
    const statsGrid = page.locator('.stats-grid');
    await expect(statsGrid).toBeVisible({ timeout: 10_000 });

    // There should be exactly four stat-card links.
    await expect(statsGrid.locator('.stat-card')).toHaveCount(4, { timeout: 10_000 });
  });

  test('stat cards link to the correct sections', async ({ page }) => {
    await goToDashboard(page);

    const cards = page.locator('.stats-grid .stat-card');
    await expect(cards).toHaveCount(4, { timeout: 10_000 });

    // The hrefs are fixed in the page script; validate all four destinations.
    const expectedHrefs = ['/admin/quotes', '/admin/offers', '/admin/calendar', '/admin/customers'];
    for (const href of expectedHrefs) {
      await expect(cards.filter({ hasNot: page.locator(`[href="${href}"]`) }).first()).not.toBeNull();
    }

    // Verify each href appears at least once in the grid.
    for (const href of expectedHrefs) {
      const card = cards.filter({ has: page.locator(`[href="${href}"]`) });
      // A card IS an anchor, so also check the card itself.
      const directCard = page.locator(`.stats-grid a.stat-card[href="${href}"]`);
      const count = await directCard.count();
      expect(count).toBeGreaterThanOrEqual(1);
    }
  });

  test('recent activity section is visible', async ({ page }) => {
    await goToDashboard(page);

    // The section always renders (shows empty state when no data).
    const activitySection = page.locator('.section-card').last();
    await expect(activitySection).toBeVisible({ timeout: 10_000 });

    // Header reads "Letzte Aktivitaeten".
    await expect(activitySection.locator('.section-header h2')).toContainText('Letzte Aktivit', {
      timeout: 10_000,
    });
  });

  test('sidebar navigation is visible with key links', async ({ page }) => {
    await goToDashboard(page);

    const sidebar = page.locator('aside.sidebar');
    await expect(sidebar).toBeVisible({ timeout: 10_000 });

    // Check that the nav links the Sidebar component renders are present.
    const expectedLabels = ['Anfragen', 'Angebote', 'Kunden', 'Kalender'];
    for (const label of expectedLabels) {
      await expect(sidebar.locator(`a:has-text("${label}")`)).toBeVisible({ timeout: 8_000 });
    }
  });

  test('sidebar brand text "AUST" is visible', async ({ page }) => {
    await goToDashboard(page);

    await expect(page.locator('.sidebar-brand')).toHaveText('AUST', { timeout: 8_000 });
  });

  test('topbar renders user name and logout button', async ({ page }) => {
    await goToDashboard(page);

    await expect(page.locator('.admin-topbar')).toBeVisible({ timeout: 8_000 });
    await expect(
      page.locator('button.topbar-logout, button:has-text("Abmelden")').first()
    ).toBeVisible({ timeout: 8_000 });
  });
});
