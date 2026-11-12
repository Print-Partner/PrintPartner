import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect } from "react";
import { RouterProvider, type createBrowserRouter } from "react-router";
import { isAuthPath, signInFor } from "./auth/next";
import { keepsEdits, signedOut } from "./auth/SignedOutNote";
import { onUnauthorized } from "./core/client";

type AppRouter = ReturnType<typeof createBrowserRouter>;

export function createQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { refetchOnWindowFocus: false } } });
}

/** An opted-in write (`KEEP_EDITS`) is in flight or was refused with 401: its edit is unsaved. */
const unsavedWrite = (queryClient: QueryClient) =>
  queryClient
    .getMutationCache()
    .getAll()
    .some(
      ({ options, state }) =>
        keepsEdits(options.meta) &&
        (state.status === "pending" ||
          (state.status === "error" && signedOut(state.error))),
    );

/**
 * A 401 means the owner is signed out: open the sign-in screen, which returns to this page
 * afterwards. Reads report through `fetchCore`; mutations through the mutation cache. A
 * mutation marked `KEEP_EDITS` (the Plan's header save and autosave) never navigates, so
 * its unsaved edit stays on screen with `SignedOutNote`; while one is pending or refused,
 * read 401s (such as the refetch after the failed save) do not navigate either. Every other
 * mutation's 401 opens sign-in as before. Sign-in clears the cache. Provisional (GRE-1429
 * review). The two account screens handle their own answers.
 */
function useSignInOnUnauthorized(router: AppRouter, queryClient: QueryClient) {
  useEffect(() => {
    const signIn = () => {
      const location = router.state.location;
      if (isAuthPath(location.pathname) || unsavedWrite(queryClient)) return;
      void router.navigate(signInFor(location), { replace: true });
    };
    const reads = onUnauthorized(({ write }) => {
      if (!write) signIn();
    });
    const writes = queryClient.getMutationCache().subscribe((event) => {
      if (event.type !== "updated" || event.action.type !== "error") return;
      if (signedOut(event.action.error) && !keepsEdits(event.mutation.options.meta)) signIn();
    });
    return () => {
      reads();
      writes();
    };
  }, [router, queryClient]);
}

export function App({ router, queryClient }: { router: AppRouter; queryClient: QueryClient }) {
  useSignInOnUnauthorized(router, queryClient);
  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  );
}
