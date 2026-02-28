import type { Page } from '@playwright/test';

/**
 * Pre-generated JWT for the staging test user.
 *
 * Claims:
 *   sub:   00000000-0000-0000-0000-000000000001
 *   email: staging@test.com
 *   role:  admin
 *   iat:   2023-11-14 (1700000000)
 *   exp:   2030-01-01 (1893456000)
 *
 * Signed with HS256 using secret:
 *   "staging-jwt-secret-do-not-use-in-production-min32chars"
 *
 * Regenerate when secret changes:
 *   python3 -c "
 *   import base64, hashlib, hmac, json
 *   secret = b'staging-jwt-secret-do-not-use-in-production-min32chars'
 *   header  = base64.urlsafe_b64encode(b'{\"alg\":\"HS256\",\"typ\":\"JWT\"}').rstrip(b'=').decode()
 *   payload = base64.urlsafe_b64encode(json.dumps({\"sub\":\"00000000-0000-0000-0000-000000000001\",\"email\":\"staging@test.com\",\"role\":\"admin\",\"iat\":1700000000,\"exp\":1893456000}).encode()).rstrip(b'=').decode()
 *   msg = f'{header}.{payload}'.encode()
 *   sig = base64.urlsafe_b64encode(hmac.new(secret, msg, hashlib.sha256).digest()).rstrip(b'=').decode()
 *   print(f'{header}.{payload}.{sig}')
 *   "
 */
export const TEST_JWT =
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9' +
  '.eyJzdWIiOiAiMDAwMDAwMDAtMDAwMC0wMDAwLTAwMDAtMDAwMDAwMDAwMDAxIiwgImVtYWlsIjogInN0YWdpbmdAdGVzdC5jb20iLCAicm9sZSI6ICJhZG1pbiIsICJpYXQiOiAxNzAwMDAwMDAwLCAiZXhwIjogMTg5MzQ1NjAwMH0' +
  '.yEW82wFGZAE6zXYd64U5gJEV6zFP7Kh2b2w9E_X-M14';

/**
 * The localStorage key used by the auth store to persist the JWT access token.
 * Defined in frontend/src/lib/stores/auth.svelte.ts as TOKEN_KEY.
 */
export const TOKEN_KEY = 'aust_access_token';

/**
 * The localStorage key used to persist the decoded user profile object.
 * Defined in frontend/src/lib/stores/auth.svelte.ts as USER_KEY.
 */
export const USER_KEY = 'aust_user';

/**
 * Injects the test JWT into localStorage so the SPA auth guard considers
 * the session authenticated, then reloads the page to let the store rehydrate.
 *
 * Call this in a beforeEach hook after navigating to any admin URL so that the
 * SPA shell is loaded first (localStorage is only available in a browser context,
 * so we need at least one navigation before we can call page.evaluate).
 *
 * @param page - The Playwright Page object for the current test context
 */
export async function injectAuth(page: Page): Promise<void> {
  await page.evaluate(
    ({ token, tokenKey, userKey }) => {
      localStorage.setItem(tokenKey, token);
      localStorage.setItem(
        userKey,
        JSON.stringify({ email: 'staging@test.com', name: 'Staging Admin', role: 'admin' })
      );
    },
    { token: TEST_JWT, tokenKey: TOKEN_KEY, userKey: USER_KEY }
  );
}
