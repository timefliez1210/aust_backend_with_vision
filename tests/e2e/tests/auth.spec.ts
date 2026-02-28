import { test, expect } from '@playwright/test';
import { injectAuth, TOKEN_KEY, USER_KEY } from './helpers';

// ---------------------------------------------------------------------------
// Auth / login flow
// ---------------------------------------------------------------------------

test.describe('Authentication', () => {
  test('unauthenticated visit to /admin redirects to login page', async ({ page }) => {
    // Make sure no stale token is present before this test.
    await page.goto('/admin/login');
    await page.evaluate((key) => localStorage.removeItem(key), TOKEN_KEY);

    await page.goto('/admin');
    await page.waitForLoadState('networkidle');

    // The layout guard redirects unauthenticated users to /admin/login.
    // The SPA may serve admin.html for all routes, so check either the URL
    // changed or the login form is visible.
    const url = page.url();
    const loginFormVisible = await page.locator('input#email').isVisible();
    const onLoginRoute = url.includes('/admin/login');

    expect(onLoginRoute || loginFormVisible).toBe(true);
  });

  test('login page renders email and password inputs', async ({ page }) => {
    await page.goto('/admin/login');
    await page.waitForLoadState('networkidle');

    await expect(page.locator('input#email')).toBeVisible();
    await expect(page.locator('input#password')).toBeVisible();
    await expect(page.locator('button[type="submit"]')).toBeVisible();
  });

  test('login page heading says "AUST Admin"', async ({ page }) => {
    await page.goto('/admin/login');
    await page.waitForLoadState('networkidle');

    await expect(page.locator('h1')).toHaveText('AUST Admin');
  });

  test('wrong credentials show an error message', async ({ page }) => {
    await page.goto('/admin/login');
    await page.waitForLoadState('networkidle');

    await page.fill('input#email', 'wrong@example.com');
    await page.fill('input#password', 'badpassword');
    await page.click('button[type="submit"]');

    // Wait for the API round-trip; the backend returns 401 and the store sets
    // auth.error, which the template renders in .login-error.
    await expect(page.locator('.login-error')).toBeVisible({ timeout: 10_000 });
  });

  test('injecting a valid JWT navigates away from login to the dashboard', async ({ page }) => {
    // Navigate first so we have a browser context with localStorage.
    await page.goto('/admin/login');
    await page.waitForLoadState('networkidle');

    await injectAuth(page);
    await page.goto('/admin');
    await page.waitForLoadState('networkidle');

    // With a valid token the layout renders the shell, not a redirect to login.
    await expect(page.locator('.dashboard, .admin-shell, h1')).toBeVisible({ timeout: 10_000 });
    expect(page.url()).not.toContain('/admin/login');
  });

  test('logout button clears session and returns to login', async ({ page }) => {
    await page.goto('/admin/login');
    await injectAuth(page);
    await page.goto('/admin');
    await page.waitForLoadState('networkidle');

    // Click the logout button in the topbar.
    const logoutBtn = page.locator('button.topbar-logout, button:has-text("Abmelden")').first();
    await expect(logoutBtn).toBeVisible({ timeout: 8_000 });
    await logoutBtn.click();

    await page.waitForLoadState('networkidle');

    // After logout the user should land on /admin/login.
    await expect(page.locator('input#email')).toBeVisible({ timeout: 8_000 });

    // localStorage token should be cleared.
    const storedToken = await page.evaluate((key) => localStorage.getItem(key), TOKEN_KEY);
    expect(storedToken).toBeNull();
  });
});
