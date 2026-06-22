import type { APIRequestContext, Page } from '@playwright/test';

/**
 * Helpers for worker-portal e2e tests.
 *
 * Seeding goes through the real REST API (admin + employee endpoints) and the
 * OTP code is read out of Mailpit, exactly like the worker actually logs in.
 * The browser session is then primed by writing the employee session token into
 * localStorage under the keys the worker store reads (`worker_token` /
 * `worker_employee` — see frontend/src/lib/stores/worker.svelte.ts).
 */

export const API_BASE = process.env.STAGING_URL || process.env.WORKER_API_BASE || 'http://localhost:8080';
export const MAILPIT_BASE = process.env.MAILPIT_BASE || 'http://localhost:8025';
export const TEST_DOMAIN = 'integration-test.invalid';
export const ADMIN_EMAIL = `admin@${TEST_DOMAIN}`;
export const ADMIN_PASSWORD = 'integration-test-password-1234';

export interface WorkerSession {
  token: string;
  employee: { id: string; first_name: string };
}

async function ok<T>(req: APIRequestContext, method: 'get' | 'post' | 'patch' | 'delete', path: string, opts: { token?: string; data?: unknown } = {}): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (opts.token) headers['Authorization'] = `Bearer ${opts.token}`;
  const res = await req[method](`${API_BASE}${path}`, { headers, data: opts.data as object });
  if (!res.ok()) throw new Error(`${method.toUpperCase()} ${path} → ${res.status()} ${await res.text()}`);
  const body = await res.text();
  return (body ? JSON.parse(body) : undefined) as T;
}

export async function adminToken(req: APIRequestContext): Promise<string> {
  const { access_token } = await ok<{ access_token: string }>(req, 'post', '/api/v1/auth/login', {
    data: { email: ADMIN_EMAIL, password: ADMIN_PASSWORD },
  });
  return access_token;
}

export async function createEmployee(req: APIRequestContext, token: string): Promise<{ id: string; email: string }> {
  const email = `mitarbeiter-${Date.now()}-${Math.random().toString(36).slice(2, 7)}@${TEST_DOMAIN}`;
  const { id } = await ok<{ id: string }>(req, 'post', '/api/v1/admin/employees', {
    token,
    data: { first_name: 'Repro', last_name: 'Helfer', email, phone: '0151 1111111' },
  });
  return { id, email };
}

/** Creates a calendar Termin and assigns the employee to it. Returns the item id. */
export async function createTerminWithEmployee(
  req: APIRequestContext,
  token: string,
  employeeId: string,
  fields: { title: string; location: string; scheduled_date: string; description?: string; start_time?: string }
): Promise<string> {
  const item = await ok<{ id: string }>(req, 'post', '/api/v1/admin/calendar-items', {
    token,
    data: {
      title: fields.title,
      location: fields.location,
      description: fields.description ?? null,
      category: 'umzug',
      scheduled_date: fields.scheduled_date,
      start_time: fields.start_time ?? '08:30:00',
      duration_hours: 8,
    },
  });
  await ok(req, 'post', `/api/v1/admin/calendar-items/${item.id}/employees`, {
    token,
    data: { employee_id: employeeId },
  });
  return item.id;
}

/** Assign an additional employee to an existing calendar Termin. */
export async function assignTerminEmployee(req: APIRequestContext, token: string, itemId: string, employeeId: string): Promise<void> {
  await ok(req, 'post', `/api/v1/admin/calendar-items/${itemId}/employees`, {
    token,
    data: { employee_id: employeeId },
  });
}

/** Admin-side seeding of a Termin assignment's clock times for one employee on one day. */
export async function setTerminHours(
  req: APIRequestContext,
  token: string,
  itemId: string,
  employeeId: string,
  dayDate: string,
  times: { clock_in: string; clock_out: string; break_minutes: number }
): Promise<void> {
  await ok(req, 'patch', `/api/v1/admin/calendar-items/${itemId}/employees/${employeeId}`, {
    token,
    data: {
      clock_in: times.clock_in,
      clock_out: times.clock_out,
      break_minutes: times.break_minutes,
      day_date: dayDate,
    },
  });
}

export async function deleteTermin(req: APIRequestContext, token: string, id: string): Promise<void> {
  await ok(req, 'delete', `/api/v1/admin/calendar-items/${id}`, { token }).catch(() => {});
}

export async function deleteEmployee(req: APIRequestContext, token: string, id: string): Promise<void> {
  await ok(req, 'post', `/api/v1/admin/employees/${id}/delete`, { token }).catch(() => {});
}

export async function createCustomer(req: APIRequestContext, token: string): Promise<{ id: string }> {
  const email = `kunde-${Date.now()}-${Math.random().toString(36).slice(2, 7)}@${TEST_DOMAIN}`;
  return ok<{ id: string }>(req, 'post', '/api/v1/admin/customers', {
    token,
    data: { email, name: 'Repro Kunde', phone: '0151 0000000', salutation: 'Frau' },
  });
}

/** Creates a Privatumzug inquiry with both addresses + a scheduled date. */
export async function createInquiry(
  req: APIRequestContext,
  token: string,
  customerId: string,
  scheduledDate: string
): Promise<{ id: string }> {
  return ok<{ id: string }>(req, 'post', '/api/v1/inquiries', {
    token,
    data: {
      customer_id: customerId,
      service_type: 'privatumzug',
      submission_mode: 'manuell',
      origin: { street: 'Teststr. 1', city: 'Hildesheim', postal_code: '31134', floor: '2', elevator: true, parking_ban: null },
      destination: { street: 'Zielweg 9', city: 'Hannover', postal_code: '30159', floor: 'EG', elevator: null, parking_ban: true },
      notes: `[${TEST_DOMAIN}] full-spec e2e`,
      scheduled_date: scheduledDate,
    },
  });
}

/** PATCH an inquiry (status / start_time / employee_notes / …). */
export async function patchInquiry(req: APIRequestContext, token: string, id: string, fields: Record<string, unknown>): Promise<void> {
  await ok(req, 'patch', `/api/v1/inquiries/${id}`, { token, data: fields });
}

/** Replace the estimation items (furniture list) on the inquiry's latest estimation. */
export async function setInquiryItems(
  req: APIRequestContext,
  token: string,
  id: string,
  items: { name: string; volume_m3: number; quantity: number }[]
): Promise<void> {
  await ok(req, 'put', `/api/v1/inquiries/${id}/items`, {
    token,
    data: { items: items.map((it) => ({ ...it, confidence: 1.0 })) },
  });
}

/** Assign an employee to the inquiry (crew on the move). */
export async function assignInquiryEmployee(req: APIRequestContext, token: string, inquiryId: string, employeeId: string, notes: string): Promise<void> {
  await ok(req, 'post', `/api/v1/inquiries/${inquiryId}/employees`, { token, data: { employee_id: employeeId, notes } });
}

/**
 * Admin-side seeding of a single assignment day's clock times (worked hours).
 * Mirrors the admin inquiry detail inline edit (`PATCH .../employees/{emp}`),
 * scoping to one `job_date` so multi-day inquiries get per-day hours.
 */
export async function setAssignmentHours(
  req: APIRequestContext,
  token: string,
  inquiryId: string,
  employeeId: string,
  dayDate: string,
  times: { clock_in: string; clock_out: string; break_minutes: number }
): Promise<void> {
  await ok(req, 'patch', `/api/v1/inquiries/${inquiryId}/employees/${employeeId}`, {
    token,
    data: {
      clock_in: times.clock_in,
      clock_out: times.clock_out,
      break_minutes: times.break_minutes,
      day_date: dayDate,
    },
  });
}

export async function deleteInquiry(req: APIRequestContext, token: string, id: string): Promise<void> {
  await ok(req, 'delete', `/api/v1/inquiries/${id}`, { token }).catch(() => {});
}

/** Primes a browser context with an admin session (key from auth.svelte.ts). */
export async function injectAdminAuth(page: Page, accessToken: string): Promise<void> {
  await page.addInitScript(({ token }) => {
    localStorage.setItem('aust_access_token', token);
    localStorage.setItem('aust_user', JSON.stringify({ email: 'admin@integration-test.invalid', name: 'Test Admin', role: 'admin' }));
  }, { token: accessToken });
}

/** Full worker OTP login: request a code, read it from Mailpit, verify.
 *  The auth endpoint is rate-limited (10/60s shared); retry on 429 so worker
 *  specs don't flake when run together. */
export async function workerOtpLogin(req: APIRequestContext, email: string): Promise<WorkerSession> {
  for (let attempt = 0; ; attempt++) {
    const res = await req.post(`${API_BASE}/api/v1/employee/auth/request`, {
      headers: { 'Content-Type': 'application/json' },
      data: { email },
    });
    if (res.ok()) break;
    if (res.status() === 429 && attempt < 2) {
      await new Promise((r) => setTimeout(r, 62_000));
      continue;
    }
    throw new Error(`POST /employee/auth/request → ${res.status()} ${await res.text()}`);
  }

  const deadline = Date.now() + 20_000;
  let code: string | null = null;
  while (Date.now() < deadline && !code) {
    const search = await req.get(`${MAILPIT_BASE}/api/v1/search?query=${encodeURIComponent(`to:"${email}"`)}`);
    if (search.ok()) {
      const { messages } = (await search.json()) as { messages: { ID: string; Subject: string }[] };
      const m = (messages ?? []).find((x) => x.Subject.includes('Zugangscode'));
      if (m) {
        const detail = await (await req.get(`${MAILPIT_BASE}/api/v1/message/${m.ID}`)).json();
        code = ((detail as { Text: string }).Text.match(/Zugangscode lautet:\s*(\d{6})/) ?? [])[1] ?? null;
      }
    }
    if (!code) await new Promise((r) => setTimeout(r, 400));
  }
  if (!code) throw new Error(`No OTP email for ${email} in Mailpit`);

  return ok<WorkerSession>(req, 'post', '/api/v1/employee/auth/verify', { data: { email, code } });
}

/** Primes the browser with a worker session (token + profile) like a logged-in worker. */
export async function injectWorkerAuth(page: Page, session: WorkerSession): Promise<void> {
  await page.addInitScript(({ token, employee }) => {
    localStorage.setItem('worker_token', token);
    localStorage.setItem('worker_employee', JSON.stringify(employee));
  }, session);
}
