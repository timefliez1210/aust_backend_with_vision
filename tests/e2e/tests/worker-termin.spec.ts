import { test, expect, devices, request as playwrightRequest } from '@playwright/test';
import type { APIRequestContext } from '@playwright/test';
import {
  adminToken,
  createEmployee,
  createTerminWithEmployee,
  deleteEmployee,
  deleteTermin,
  workerOtpLogin,
  injectWorkerAuth,
  type WorkerSession,
} from './worker-helpers';

/**
 * REPRODUCTION — "the workers can not open the jobs with the details
 * (description, start time, notes); nothing happens on tap."
 *
 * When a move is scheduled as a calendar **Termin** (calendar_item) and the
 * crew is assigned to it, the worker schedule renders it as `entry_type: 'item'`.
 * In frontend/src/routes/worker/schedule/+page.svelte that branch is a plain
 * <div class="job-card item-card"> with NO onclick — only inquiry assignments
 * render the clickable <button> that navigates to /worker/jobs/[id].
 *
 * Result on a real phone: the card is a dead end. Tapping does nothing, there
 * is no detail view, and the addresses/description/start time/notes are
 * unreachable. (Desktop or programmatic clicks hide nothing here — the card
 * genuinely has no handler — but a touch `tap()` mirrors what the movers do.)
 *
 * This test drives the real worker portal on an emulated phone and expects a
 * Termin to be openable like a job. It FAILS today, reproducing the report.
 */
// video is off here: the shared config records on failure, which needs an
// ffmpeg binary that isn't installed on dev boxes; the screenshot is enough.
test.use({ ...devices['Pixel 5'], channel: 'chrome', video: 'off' });

// No retries: this is a reproduction (expected to fail until fixed), and the
// worker OTP request endpoint is rate-limited (10/min) — a retry just re-runs
// beforeAll into a 429 and muddies the result.
test.describe.configure({ retries: 0 });

const FRONT = process.env.FRONTEND_URL || 'http://localhost:4173';

let api: APIRequestContext;
let token: string;
let employeeId: string;
let terminId: string;
let session: WorkerSession;

const TITLE = 'Umzug Familie Müller';
const ROUTE = 'Teststr. 1, 31134 Hildesheim → Zielweg 9, 30159 Hannover';
const DESCRIPTION = 'Klavier im Wohnzimmer, 2. OG ohne Aufzug. Beginn 08:30.';

function jobDate(): string {
  const n = new Date();
  return `${n.getFullYear()}-${String(n.getMonth() + 1).padStart(2, '0')}-15`;
}

test.beforeAll(async () => {
  api = await playwrightRequest.newContext();
  token = await adminToken(api);
  const emp = await createEmployee(api, token);
  employeeId = emp.id;
  terminId = await createTerminWithEmployee(api, token, employeeId, {
    title: TITLE,
    location: ROUTE,
    description: DESCRIPTION,
    scheduled_date: jobDate(),
    start_time: '08:30:00',
  });
  session = await workerOtpLogin(api, emp.email);
});

test.afterAll(async () => {
  await deleteTermin(api, token, terminId);
  await deleteEmployee(api, token, employeeId);
  await api.dispose();
});

test('a worker can open a Termin from the schedule and see its details', async ({ page }) => {
  await injectWorkerAuth(page, session);

  await page.goto(`${FRONT}/worker/schedule`);
  await page.waitForLoadState('networkidle');

  // The Termin shows up in the schedule…
  const card = page.locator('.job-card', { hasText: TITLE });
  await expect(card).toBeVisible();

  // Tapping it must open the Termin detail (before the fix it was a dead <div>).
  await card.tap();
  await page.waitForTimeout(1000);

  // The worker lands on the Termin detail and sees the location, the start
  // time and the description/notes — what they need to do the job.
  expect(page.url(), 'tapping the Termin did not navigate — the card is a dead end').toContain('/worker/items/');
  await expect(page.getByText(ROUTE)).toBeVisible();         // location / addresses
  await expect(page.getByText('08:30 Uhr')).toBeVisible();   // planned start time
  await expect(page.getByText(DESCRIPTION)).toBeVisible();   // description / notes
});
