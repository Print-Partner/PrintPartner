import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useEffect } from "react";
import { RouterProvider, type createBrowserRouter } from "react-router";
import { isAuthPath, signInFor } from "./auth/next";
import { onUnauthorized } from "./core/client";

type AppRouter = ReturnType<typeof createBrowserRouter>;

export function createQueryClient(): QueryClient {
  return new QueryClient({ defaultOptions: { queries: { refetchOnWindowFocus: false } } });
}

/**
 * A 401 from any read or write means the owner is signed out: open the sign-in screen,
 * which returns to this page afterwards. The two account screens handle their own
 * answers, such as a wrong password.
 */
function useSignInOnUnauthorized(router: AppRouter) {
  useEffect(
    () =>
      onUnauthorized(() => {
        const location = router.state.location;
        if (isAuthPath(location.pathname)) return;
        void router.navigate(signInFor(location), { replace: true });
      }),
    [router],
  );
}

export function App({ router, queryClient }: { router: AppRouter; queryClient: QueryClient }) {
  useSignInOnUnauthorized(router);
  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  );
}
