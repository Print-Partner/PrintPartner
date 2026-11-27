import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Link, Navigate, useLocation, useNavigate, useSearchParams } from "react-router";
import { CoreError } from "../core/client";
import { readCore, writeCore, writeCoreAnswer } from "../core/rust";
import { routes } from "../routes";
import {
  MISMATCH,
  OWNER_EXISTS,
  OwnerExists,
  PASSWORD_TOO_SHORT,
  RESET_INVALID,
  RESET_REQUESTED,
  recoveryProblem,
  setupProblem,
  signInProblem,
  statusProblem,
} from "./messages";
import { FORGOT_PASSWORD, HOME, RESET_PASSWORD, SETUP, SIGN_IN, nextPath, withNext } from "./next";

/** Installation-wide auth status. Never cached between visits. */
function useAuthStatus() {
  return useQuery({
    queryKey: ["auth", "status"],
    queryFn: ({ signal }) => readCore("readAuthStatus", {}, signal),
    retry: false,
    gcTime: 0,
    // A stopped core is the answer to show, so the read runs while the browser reports
    // no network too.
    networkMode: "always",
  });
}

type Status = ReturnType<typeof useAuthStatus>;

function AuthLayout({
  title,
  status,
  children,
}: {
  title: string;
  status: Status;
  children: ReactNode;
}) {
  const id = useId();
  const heading = useRef<HTMLHeadingElement>(null);
  // Arriving here is a page change, often a redirect: start reading at the heading.
  useEffect(() => heading.current?.focus(), []);
  return (
    <main
      id="main"
      className="pp-auth"
      aria-labelledby={id}
      data-ready={status.isPending ? undefined : ""}
    >
      <p className="pp-name">Print Partner</p>
      <h1 id={id} ref={heading} tabIndex={-1}>
        {title}
      </h1>
      {children}
    </main>
  );
}

/** The status read's failure, with a retry. Null while it is loading or loaded. */
function StatusProblem({ status }: { status: Status }) {
  if (status.isPending) return <p role="status">Checking Print Partner…</p>;
  if (!status.isError) return null;
  return (
    <>
      <p role="alert" className="pp-auth-problem">
        {statusProblem(status.error)}
      </p>
      <button type="button" className="pp-button" onClick={() => void status.refetch()}>
        Try again
      </button>
    </>
  );
}

function Field({
  label,
  name,
  type,
  autoComplete,
  invalid,
  describedBy,
  hint,
}: {
  label: string;
  name: string;
  type: "email" | "password";
  autoComplete: string;
  invalid?: boolean;
  describedBy?: string;
  hint?: string;
}) {
  const inputId = useId();
  const hintId = useId();
  const described = [hint && hintId, describedBy].filter(Boolean).join(" ") || undefined;
  return (
    <div className="pp-field">
      <label htmlFor={inputId}>{label}</label>
      <input
        id={inputId}
        name={name}
        type={type}
        autoComplete={autoComplete}
        required
        aria-invalid={invalid || undefined}
        aria-describedby={described}
      />
      {hint && (
        <span id={hintId} className="pp-auth-hint">
          {hint}
        </span>
      )}
    </div>
  );
}

/** Runs one submit at a time and keeps its failure text. */
function useSubmit(send: (form: FormData) => Promise<void>, describe: (error: unknown) => string) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (busy) return;
    const form = new FormData(event.currentTarget);
    setBusy(true);
    setProblem(null);
    send(form)
      .catch((error: unknown) => setProblem(describe(error)))
      .finally(() => setBusy(false));
  };
  return { busy, problem, onSubmit };
}

const text = (form: FormData, name: string) => {
  const value = form.get(name);
  return typeof value === "string" ? value : "";
};

/** Sign-in for the owner account; returns to the page in `next`. */
export function LoginScreen() {
  const [params] = useSearchParams();
  const location = useLocation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const status = useAuthStatus();
  const problemId = useId();
  const next = nextPath(params.get("next"), routes);
  const signedOut = (location.state as { signedOut?: boolean } | null)?.signedOut === true;
  const submit = useSubmit(async (form) => {
    await writeCore("signIn", {}, { email: text(form, "email"), password: text(form, "password") });
    // Reads that failed while signed out start over.
    queryClient.clear();
    await navigate(next, { replace: true });
  }, signInProblem);

  if (status.data?.setup_required) return <Navigate to={`${SETUP}${location.search}`} replace />;
  return (
    <AuthLayout title="Sign in" status={status}>
      {signedOut && <p role="status">You signed out.</p>}
      <StatusProblem status={status} />
      {status.isSuccess && (
        <form className="pp-auth-form" onSubmit={submit.onSubmit}>
          <Field label="Email" name="email" type="email" autoComplete="username" />
          <Field
            label="Password"
            name="password"
            type="password"
            autoComplete="current-password"
            describedBy={submit.problem ? problemId : undefined}
          />
          {submit.problem && (
            <p id={problemId} role="alert" className="pp-auth-problem">
              {submit.problem}
            </p>
          )}
          <button
            type="submit"
            className="pp-button pp-button-primary"
            aria-disabled={submit.busy || undefined}
          >
            {submit.busy ? "Signing in…" : "Sign in"}
          </button>
        </form>
      )}
      {status.isSuccess && (
        <Link to={withNext(FORGOT_PASSWORD, next === HOME ? undefined : next)}>Forgot password?</Link>
      )}
    </AuthLayout>
  );
}

/** First run: create the owner account, then open Today. */
export function SetupScreen() {
  const location = useLocation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const status = useAuthStatus();
  const problemId = useId();
  const [mismatch, setMismatch] = useState(false);
  const submit = useSubmit(async (form) => {
    const password = text(form, "password");
    if (password !== text(form, "confirm")) {
      setMismatch(true);
      throw new Error(MISMATCH);
    }
    setMismatch(false);
    await writeCore("registerOwner", {}, { email: text(form, "email"), password }).catch(
      async (error: unknown) => {
        // The register handler and the auth guard both answer 403. Only a status read that
        // now finds the owner account says the handler refused because it exists.
        if (!(error instanceof CoreError) || error.status !== 403) throw error;
        const status = await readCore("readAuthStatus", {}).catch(() => null);
        throw status?.setup_required === false ? new OwnerExists() : error;
      },
    );
    queryClient.clear();
    await navigate(HOME, { replace: true });
  }, (error) => (error instanceof Error && error.message === MISMATCH ? MISMATCH : setupProblem(error)));

  if (status.data?.setup_required === false) {
    return <Navigate to={`${SIGN_IN}${location.search}`} replace />;
  }
  return (
    <AuthLayout title="Set up Print Partner" status={status}>
      <StatusProblem status={status} />
      {status.isSuccess && (
        <form className="pp-auth-form" onSubmit={submit.onSubmit}>
          <p>Create the owner account. You sign in with this email and password.</p>
          <Field label="Email" name="email" type="email" autoComplete="username" />
          <Field
            label="Password"
            name="password"
            type="password"
            autoComplete="new-password"
            hint="At least 8 characters."
          />
          <Field
            label="Password again"
            name="confirm"
            type="password"
            autoComplete="new-password"
            invalid={mismatch}
            describedBy={submit.problem ? problemId : undefined}
          />
          {submit.problem && (
            <p id={problemId} role="alert" className="pp-auth-problem">
              {submit.problem}
            </p>
          )}
          {submit.problem === OWNER_EXISTS && (
            <Link to={SIGN_IN}>Go to sign-in</Link>
          )}
          <button
            type="submit"
            className="pp-button pp-button-primary"
            aria-disabled={submit.busy || undefined}
          >
            {submit.busy ? "Creating account…" : "Create account"}
          </button>
        </form>
      )}
    </AuthLayout>
  );
}

export function ForgotPasswordScreen() {
  const [params] = useSearchParams();
  const next = nextPath(params.get("next"), routes);
  const status = useAuthStatus();
  const available = status.data?.recovery_email_available === true;
  const [requested, setRequested] = useState(false);
  const submit = useSubmit(async (form) => {
    await writeCore("requestPasswordReset", {}, { email: text(form, "email") });
    setRequested(true);
  }, (error) => recoveryProblem(error, false));

  return (
    <AuthLayout title="Reset your password" status={status}>
      <StatusProblem status={status} />
      {status.isSuccess && !available && (
        <p>Password reset email is not configured for this installation.</p>
      )}
      {available && !requested && (
        <form className="pp-auth-form" onSubmit={submit.onSubmit}>
          <p>Enter the email address you use to sign in.</p>
          <Field label="Email" name="email" type="email" autoComplete="email" />
          {submit.problem && <p role="alert" className="pp-auth-problem">{submit.problem}</p>}
          <button
            type="submit"
            className="pp-button pp-button-primary"
            aria-disabled={submit.busy || undefined}
          >
            {submit.busy ? "Requesting link…" : "Request reset link"}
          </button>
        </form>
      )}
      {requested && <p role="status">{RESET_REQUESTED}</p>}
      <Link to={withNext(SIGN_IN, next === HOME ? undefined : next)}>Back to sign-in</Link>
    </AuthLayout>
  );
}

export function ResetPasswordScreen() {
  const { search, hash } = useLocation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [token] = useState(() => new URLSearchParams(search).get("token")?.trim());
  const status = useAuthStatus();
  const problemId = useId();
  const [validation, setValidation] = useState<string | null>(null);
  const submit = useSubmit(async (form) => {
    if (!token) return;
    const password = text(form, "password");
    const problem = password.length < 8
      ? PASSWORD_TOO_SHORT
      : password !== text(form, "confirm") ? MISMATCH : null;
    setValidation(problem);
    if (problem) return;
    await writeCoreAnswer("redeemPasswordReset", {}, { redirect: "error" }, { token, password });
    queryClient.clear();
    await navigate(HOME, { replace: true });
  }, (error) => recoveryProblem(error, true));

  // Keep the credential only in this screen, not in the current history entry.
  useEffect(() => {
    if (search || hash) void navigate(RESET_PASSWORD, { replace: true });
  }, [search, hash, navigate]);

  const problem = validation ?? submit.problem;
  const invalid = !token || submit.problem === RESET_INVALID;
  return (
    <AuthLayout title="Choose a new password" status={status}>
      {invalid ? (
        <>
          <p role="alert" className="pp-auth-problem">{RESET_INVALID}</p>
          <Link to={FORGOT_PASSWORD}>Request a new reset link</Link>
        </>
      ) : (
        <>
          <StatusProblem status={status} />
          {status.isSuccess && (
            <form className="pp-auth-form" onSubmit={submit.onSubmit}>
              <Field
                label="New password"
                name="password"
                type="password"
                autoComplete="new-password"
                hint="At least 8 characters."
                invalid={validation === PASSWORD_TOO_SHORT}
                describedBy={validation === PASSWORD_TOO_SHORT ? problemId : undefined}
              />
              <Field
                label="New password again"
                name="confirm"
                type="password"
                autoComplete="new-password"
                invalid={validation === MISMATCH}
                describedBy={validation === MISMATCH ? problemId : undefined}
              />
              {problem && <p id={problemId} role="alert" className="pp-auth-problem">{problem}</p>}
              <button
                type="submit"
                className="pp-button pp-button-primary"
                aria-disabled={submit.busy || undefined}
              >
                {submit.busy ? "Resetting password…" : "Reset password"}
              </button>
            </form>
          )}
        </>
      )}
      <Link to={SIGN_IN}>Back to sign-in</Link>
    </AuthLayout>
  );
}
