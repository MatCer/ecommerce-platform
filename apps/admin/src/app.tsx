import { LoadingState, ToastRegion } from "@platform/ui";
import { Navigate, Route, Router, type RouteSectionProps, useLocation } from "@solidjs/router";
import { QueryClientProvider } from "@tanstack/solid-query";
import { lazy, Match, onMount, Switch } from "solid-js";
import { ReauthDialog } from "./components/ReauthDialog.tsx";
import { Shell } from "./components/Shell.tsx";
import { t } from "./i18n/index.ts";
import { queryClient } from "./lib/api.ts";
import { bootstrapSession, status } from "./lib/session.ts";
import { Login } from "./pages/Login.tsx";
import { ResetPassword } from "./pages/ResetPassword.tsx";

const Dashboard = lazy(() => import("./pages/Dashboard.tsx"));
const Products = lazy(() => import("./pages/Products.tsx"));
const ProductEditor = lazy(() => import("./pages/ProductEditor.tsx"));
const Categories = lazy(() => import("./pages/Categories.tsx"));
const Parameters = lazy(() => import("./pages/Parameters.tsx"));
const Markets = lazy(() => import("./pages/Markets.tsx"));
const Staff = lazy(() => import("./pages/Staff.tsx"));
const AuditLog = lazy(() => import("./pages/AuditLog.tsx"));
const Security = lazy(() => import("./pages/Security.tsx"));
const PriceLists = lazy(() => import("./pages/PriceLists.tsx"));
const Sales = lazy(() => import("./pages/Sales.tsx"));
const Coupons = lazy(() => import("./pages/Coupons.tsx"));
const Inventory = lazy(() => import("./pages/Inventory.tsx"));
const TaxProfile = lazy(() => import("./pages/TaxProfile.tsx"));
const ContentPages = lazy(() => import("./pages/ContentPages.tsx"));
const ContentEditor = lazy(() => import("./pages/ContentEditor.tsx"));
const ContentMenus = lazy(() => import("./pages/ContentMenus.tsx"));
const ContentLegal = lazy(() => import("./pages/ContentLegal.tsx"));
const Imports = lazy(() => import("./pages/Imports.tsx"));
const ExportFeeds = lazy(() => import("./pages/ExportFeeds.tsx"));
const SearchSynonyms = lazy(() => import("./pages/SearchSynonyms.tsx"));
const NotFound = lazy(() => import("./pages/NotFound.tsx"));

function Root(props: RouteSectionProps) {
  return (
    <>
      {props.children}
      <ReauthDialog />
      <ToastRegion label={t("app.notifications")} />
    </>
  );
}

/** Signed-in area: sends everyone else to the sign-in page, remembering where they were. */
function Protected(props: RouteSectionProps) {
  const location = useLocation();
  const next = () => encodeURIComponent(location.pathname + location.search);
  return (
    <Switch>
      <Match when={status() === "loading"}>
        <LoadingState label={t("common.loading")} />
      </Match>
      <Match when={status() === "signed-out"}>
        <Navigate href={`/login?next=${next()}`} />
      </Match>
      <Match when={status() === "signed-in"}>
        <Shell>{props.children}</Shell>
      </Match>
    </Switch>
  );
}

export function App() {
  onMount(() => void bootstrapSession());
  return (
    <QueryClientProvider client={queryClient}>
      <Router root={Root}>
        <Route path="/login" component={Login} />
        <Route path="/reset-password" component={ResetPassword} />
        <Route path="/" component={Protected}>
          <Route path="/" component={Dashboard} />
          <Route path="/products" component={Products} />
          <Route path="/products/new" component={ProductEditor} />
          <Route path="/products/:id" component={ProductEditor} />
          <Route path="/categories" component={Categories} />
          <Route path="/parameters" component={Parameters} />
          <Route path="/inventory" component={Inventory} />
          <Route path="/price-lists" component={PriceLists} />
          <Route path="/sales" component={Sales} />
          <Route path="/coupons" component={Coupons} />
          <Route path="/markets" component={Markets} />
          <Route path="/content/pages" component={ContentPages} />
          <Route path="/content/blog" component={ContentPages} />
          <Route path="/content/pages/new" component={ContentEditor} />
          <Route path="/content/pages/:id" component={ContentEditor} />
          <Route path="/content/blog/new" component={ContentEditor} />
          <Route path="/content/blog/:id" component={ContentEditor} />
          <Route path="/content/menus" component={ContentMenus} />
          <Route path="/content/legal" component={ContentLegal} />
          <Route path="/imports" component={Imports} />
          <Route path="/imports/:id" component={Imports} />
          <Route path="/feeds" component={ExportFeeds} />
          <Route path="/settings/search" component={SearchSynonyms} />
          <Route path="/settings/tax" component={TaxProfile} />
          <Route path="/staff" component={Staff} />
          <Route path="/audit-log" component={AuditLog} />
          <Route path="/account/security" component={Security} />
          <Route path="*" component={NotFound} />
        </Route>
      </Router>
    </QueryClientProvider>
  );
}
