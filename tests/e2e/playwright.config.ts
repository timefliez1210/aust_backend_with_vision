import { defineConfig, devices } from '@playwright/test';

const FRONTEND_URL = process.env.FRONTEND_URL || 'http://localhost:4173';
const STAGING_URL = process.env.STAGING_URL || 'http://localhost:8099';

export default defineConfig({
  testDir: './tests',
  timeout: 30_000,
  retries: 1,
  fullyParallel: false,

  use: {
    baseURL: FRONTEND_URL,
    headless: true,
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },

  projects: [
    {
      name: 'chromium',
      // Use the system-installed Chrome so dev/CI boxes without the Playwright
      // browser bundle (`npx playwright install`) can still run the suite.
      use: { ...devices['Desktop Chrome'], channel: 'chrome' },
    },
    {
      // The worker portal is a phone-first app. This project emulates a real
      // touch device so taps go through tap() (pointer/touch), not a mouse
      // click — the exact difference that reveals the dead Termin-card bug.
      name: 'mobile',
      use: { ...devices['Pixel 5'], channel: 'chrome' },
    },
  ],
});
