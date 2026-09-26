// Shared accessible UI primitives styled after GitLab Pajamas (Kobalte + the tokens in
// @platform/config/tailwind/theme.css). Guide: docs/design/pajamas.md.
// Apps must let Tailwind scan this package: `@source "<path>/packages/ui/src";`.
export { Badge, type Tone } from "./badge.tsx";
export {
  Button,
  type ButtonCategory,
  ButtonGroup,
  type ButtonProps,
  type ButtonSize,
  type ButtonStyle,
  type ButtonVariant,
  buttonClass,
} from "./button.tsx";
export {
  Checkbox,
  controlClass,
  describedBy,
  errorClass,
  FieldGroup,
  FormGroup,
  hintClass,
  labelClass,
  Radio,
  radioClass,
  SearchBox,
  SelectField,
  type SelectOption,
  TextField,
  Toggle,
} from "./field.tsx";
export { Icon, type IconName } from "./icon.tsx";
export {
  Avatar,
  Breadcrumb,
  type BreadcrumbItem,
  Card,
  Collapse,
  linkClass,
  PageHeading,
  SegmentedControl,
  type SegmentOption,
} from "./layout.tsx";
export {
  ConfirmDialog,
  Dialog,
  Drawer,
  Menu,
  type MenuItem,
  menuItemClass,
  menuPanelClass,
  showToast,
  type TabItem,
  Tabs,
  ToastRegion,
  TooltipButton,
} from "./overlay.tsx";
export {
  Alert,
  EmptyState,
  ErrorState,
  LoadingState,
  PermissionDenied,
  ProgressBar,
  Skeleton,
  Spinner,
} from "./states.tsx";
export { Th, tableClass, tdClass } from "./table.tsx";
