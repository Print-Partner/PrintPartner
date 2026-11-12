import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect } from "react";
import { RouterProvider, type createBrowserRouter } from "react-router";
import { isAuthPath, signInFor } from "./auth/next";
import { CoreError, onUnauthorized } from "./core/client";

type AppRouter = ReturnType<typeof createBrowserRouter>;

export function createQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { refetchOnWindowFocus: false } } });
}

/** A write is in flight, or failed because the session ended: its edit may be unsaved. */
const unsavedWrite = (queryClient: QueryClient) =>
  queryClient
    .getMutationCache()
    .getAll()
    .some(
      ({ state }) =>
        state.status === "pending" ||
        (state.status === "error" && state.error instanceof CoreError && state.error.status === 401),
    );

/**
 * A 401 from a read means the owner is signed out: open the sign-in screen, which returns
 * to this page afterwards. A 401 from a write never navigates, so an unsaved edit stays on
 * screen; the write's own status offers sign-in instead (`SignedOutNote`). While such a
 * write is pending or refused, read 401s (such as the refetch after the failed save) do
 * not navigate either; the owner leaves through that sign-in link. Sign-in clears the
 * cache. Provisional (GRE-1429 review). The two account screens handle their own answers.
 */
function useSignInOnUnauthorized(router: AppRouter, queryClient: QueryClient) {
  useEffect(
    () =>
      onUnauthorized(({ write }) => {
        const location = router.state.location;
        if (write || isAuthPath(location.pathname) || unsavedWrite(queryClient)) return;
        void router.navigate(signInFor(location), { replace: true });
      }),
    [router, queryClient],
  );
}

export function App({ router, queryClient }: { router: AppRouter; queryClient: QueryClient }) {
  useSignInOnUnauthorized(router, queryClient);
  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  );
}
