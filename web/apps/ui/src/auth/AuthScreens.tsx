import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Link, Navigate, useLocation, useNavigate, useSearchParams } from "react-router";
import { readCore, writeCore } from "../core/rust";
import { routes } from "../routes";
import {
  MISMATCH,
  OWNER_EXISTS,
  setupProblem,
  signInProblem,
  statusProblem,
} from "./messages";
import { HOME, SETUP, SIGN_IN, nextPath } from "./next";

/** Whether the owner account still has to be created. Never cached between visits. */
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
    await writeCore("registerOwner", {}, { email: text(form, "email"), password });
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
