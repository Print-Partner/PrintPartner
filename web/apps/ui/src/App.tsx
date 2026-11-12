import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect } from "react";
import { RouterProvider, type createBrowserRouter } from "react-router";
import { SETUP, SIGN_IN, signInFor } from "./auth/next";
import { CoreError } from "./core/client";

type AppRouter = ReturnType<typeof createBrowserRouter>;

export function createQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { refetchOnWindowFocus: false } } });
}

/**
 * A 401 from any read or write means the owner is signed out: open the sign-in
 * screen, which returns to this page afterwards. The two account screens handle
 * their own answers.
 */
function useSignInOnUnauthorized(router: AppRouter, queryClient: QueryClient) {
  useEffect(() => {
    const onError = (error: unknown) => {
      if (!(error instanceof CoreError) || error.status !== 401) return;
      const location = router.state.location;
      if (location.pathname === SIGN_IN || location.pathname === SETUP) return;
      void router.navigate(signInFor(location), { replace: true });
    };
    // "failed" is one attempt of a query that retries; a 401 does not get better on retry.
    const queries = queryClient.getQueryCache().subscribe((event) => {
      if (event.type !== "updated") return;
      if (event.action.type === "error" || event.action.type === "failed") {
        onError(event.action.error);
      }
    });
    const mutations = queryClient.getMutationCache().subscribe((event) => {
      if (event.type === "updated" && event.action.type === "error") onError(event.action.error);
    });
    return () => {
      queries();
      mutations();
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
