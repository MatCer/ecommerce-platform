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
const Redirects = lazy(() => import("./pages/Redirects.tsx"));
const Imports = lazy(() => import("./pages/Imports.tsx"));
const DataImports = lazy(() => import("./pages/DataImports.tsx"));
const ArchivedOrders = lazy(() => import("./pages/ArchivedOrders.tsx"));
const DataPrivacy = lazy(() => import("./pages/DataPrivacy.tsx"));
const ExportFeeds = lazy(() => import("./pages/ExportFeeds.tsx"));
const SearchSynonyms = lazy(() => import("./pages/SearchSynonyms.tsx"));
const Collections = lazy(() => import("./pages/Collections.tsx"));
const Recommendations = lazy(() => import("./pages/Recommendations.tsx"));
const ShippingMethods = lazy(() => import("./pages/ShippingMethods.tsx"));
const PaymentMethods = lazy(() => import("./pages/PaymentMethods.tsx"));
const PaymentExceptions = lazy(() => import("./pages/PaymentExceptions.tsx"));
const BankTransactions = lazy(() => import("./pages/BankTransactions.tsx"));
const Orders = lazy(() => import("./pages/Orders.tsx"));
const Withdrawals = lazy(() => import("./pages/Withdrawals.tsx"));
const Carriers = lazy(() => import("./pages/Carriers.tsx"));
const OrderDetail = lazy(() => import("./pages/OrderDetail.tsx"));
const AiBulkEdit = lazy(() => import("./pages/AiBulkEdit.tsx"));
const AiSettings = lazy(() => import("./pages/AiSettings.tsx"));
const Webhooks = lazy(() => import("./pages/Webhooks.tsx"));
const AdTracking = lazy(() => import("./pages/AdTracking.tsx"));
const PlatformJobs = lazy(() => import("./pages/PlatformJobs.tsx"));
const Subscribers = lazy(() => import("./pages/Subscribers.tsx"));
const Reviews = lazy(() => import("./pages/Reviews.tsx"));
const Segments = lazy(() => import("./pages/Segments.tsx"));
const Campaigns = lazy(() => import("./pages/Campaigns.tsx"));
const CampaignEditor = lazy(() => import("./pages/CampaignEditor.tsx"));
const Emails = lazy(() => import("./pages/Emails.tsx"));
const EmailBranding = lazy(() => import("./pages/EmailBranding.tsx"));
const Themes = lazy(() => import("./pages/Themes.tsx"));
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
          <Route path="/ai/bulk-edit" component={AiBulkEdit} />
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
          <Route path="/content/redirects" component={Redirects} />
          <Route path="/themes" component={Themes} />
          <Route path="/imports" component={Imports} />
          <Route path="/imports/:id" component={Imports} />
          <Route path="/data/imports" component={DataImports} />
          <Route path="/data/imports/:id" component={DataImports} />
          <Route path="/data/archived-orders" component={ArchivedOrders} />
          <Route path="/data/privacy" component={DataPrivacy} />
          <Route path="/feeds" component={ExportFeeds} />
          <Route path="/settings/search" component={SearchSynonyms} />
          <Route path="/collections" component={Collections} />
          <Route path="/settings/recommendations" component={Recommendations} />
          <Route path="/settings/ai" component={AiSettings} />
          <Route path="/orders" component={Orders} />
          <Route path="/withdrawals" component={Withdrawals} />
          <Route path="/settings/carriers" component={Carriers} />
          <Route path="/orders/:id" component={OrderDetail} />
          <Route path="/payments/exceptions" component={PaymentExceptions} />
          <Route path="/payments/bank" component={BankTransactions} />
          <Route path="/settings/shipping" component={ShippingMethods} />
          <Route path="/settings/payments" component={PaymentMethods} />
          <Route path="/settings/tax" component={TaxProfile} />
          <Route path="/staff" component={Staff} />
          <Route path="/audit-log" component={AuditLog} />
          <Route path="/settings/webhooks" component={Webhooks} />
          <Route path="/settings/ad-tracking" component={AdTracking} />
          <Route path="/settings/emails" component={Emails} />
          <Route path="/settings/email-branding" component={EmailBranding} />
          <Route path="/marketing/subscribers" component={Subscribers} />
          <Route path="/marketing/reviews" component={Reviews} />
          <Route path="/marketing/segments" component={Segments} />
          <Route path="/marketing/campaigns" component={Campaigns} />
          <Route path="/marketing/campaigns/new" component={CampaignEditor} />
          <Route path="/marketing/campaigns/:id" component={CampaignEditor} />
          <Route path="/platform/jobs" component={PlatformJobs} />
          <Route path="/account/security" component={Security} />
          <Route path="*" component={NotFound} />
        </Route>
      </Router>
    </QueryClientProvider>
  );
}
