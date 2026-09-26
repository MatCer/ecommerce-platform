// GitLab icons (@gitlab/svgs, MIT). Only the icons listed here are bundled: add an import and
// an entry to use another one (browse https://gitlab-org.gitlab.io/gitlab-svgs/).
import homeSvg from "@gitlab/svgs/dist/sprite_icons/home.svg?raw";
import listTaskSvg from "@gitlab/svgs/dist/sprite_icons/list-task.svg?raw";
import packageSvg from "@gitlab/svgs/dist/sprite_icons/package.svg?raw";
import tagSvg from "@gitlab/svgs/dist/sprite_icons/tag.svg?raw";
import bullhornSvg from "@gitlab/svgs/dist/sprite_icons/bullhorn.svg?raw";
import documentSvg from "@gitlab/svgs/dist/sprite_icons/document.svg?raw";
import exportSvg from "@gitlab/svgs/dist/sprite_icons/export.svg?raw";
import archiveSvg from "@gitlab/svgs/dist/sprite_icons/archive.svg?raw";
import settingsSvg from "@gitlab/svgs/dist/sprite_icons/settings.svg?raw";
import adminSvg from "@gitlab/svgs/dist/sprite_icons/admin.svg?raw";
import chevronDownSvg from "@gitlab/svgs/dist/sprite_icons/chevron-down.svg?raw";
import chevronRightSvg from "@gitlab/svgs/dist/sprite_icons/chevron-right.svg?raw";
import chevronLeftSvg from "@gitlab/svgs/dist/sprite_icons/chevron-left.svg?raw";
import chevronUpSvg from "@gitlab/svgs/dist/sprite_icons/chevron-up.svg?raw";
import closeSvg from "@gitlab/svgs/dist/sprite_icons/close.svg?raw";
import searchSvg from "@gitlab/svgs/dist/sprite_icons/search.svg?raw";
import checkSvg from "@gitlab/svgs/dist/sprite_icons/check.svg?raw";
import checkCircleSvg from "@gitlab/svgs/dist/sprite_icons/check-circle.svg?raw";
import informationOSvg from "@gitlab/svgs/dist/sprite_icons/information-o.svg?raw";
import warningSvg from "@gitlab/svgs/dist/sprite_icons/warning.svg?raw";
import errorSvg from "@gitlab/svgs/dist/sprite_icons/error.svg?raw";
import sidebarSvg from "@gitlab/svgs/dist/sprite_icons/sidebar.svg?raw";
import hamburgerSvg from "@gitlab/svgs/dist/sprite_icons/hamburger.svg?raw";
import ellipsisVSvg from "@gitlab/svgs/dist/sprite_icons/ellipsis_v.svg?raw";
import arrowLeftSvg from "@gitlab/svgs/dist/sprite_icons/arrow-left.svg?raw";
import plusSvg from "@gitlab/svgs/dist/sprite_icons/plus.svg?raw";
import externalLinkSvg from "@gitlab/svgs/dist/sprite_icons/external-link.svg?raw";
import sunSvg from "@gitlab/svgs/dist/sprite_icons/sun.svg?raw";
import moonSvg from "@gitlab/svgs/dist/sprite_icons/moon.svg?raw";
import earthSvg from "@gitlab/svgs/dist/sprite_icons/earth.svg?raw";
import powerSvg from "@gitlab/svgs/dist/sprite_icons/power.svg?raw";
import lockSvg from "@gitlab/svgs/dist/sprite_icons/lock.svg?raw";
import userSvg from "@gitlab/svgs/dist/sprite_icons/user.svg?raw";
import clearSvg from "@gitlab/svgs/dist/sprite_icons/clear.svg?raw";
import retrySvg from "@gitlab/svgs/dist/sprite_icons/retry.svg?raw";
import removeSvg from "@gitlab/svgs/dist/sprite_icons/remove.svg?raw";
import pencilSvg from "@gitlab/svgs/dist/sprite_icons/pencil.svg?raw";
import downloadSvg from "@gitlab/svgs/dist/sprite_icons/download.svg?raw";
import uploadSvg from "@gitlab/svgs/dist/sprite_icons/upload.svg?raw";
import filterSvg from "@gitlab/svgs/dist/sprite_icons/filter.svg?raw";
import { splitProps } from "solid-js";

const sources = {
  home: homeSvg,
  "list-task": listTaskSvg,
  package: packageSvg,
  tag: tagSvg,
  bullhorn: bullhornSvg,
  document: documentSvg,
  export: exportSvg,
  archive: archiveSvg,
  settings: settingsSvg,
  admin: adminSvg,
  "chevron-down": chevronDownSvg,
  "chevron-right": chevronRightSvg,
  "chevron-left": chevronLeftSvg,
  "chevron-up": chevronUpSvg,
  close: closeSvg,
  search: searchSvg,
  check: checkSvg,
  "check-circle": checkCircleSvg,
  "information-o": informationOSvg,
  warning: warningSvg,
  error: errorSvg,
  sidebar: sidebarSvg,
  hamburger: hamburgerSvg,
  "ellipsis_v": ellipsisVSvg,
  "arrow-left": arrowLeftSvg,
  plus: plusSvg,
  "external-link": externalLinkSvg,
  sun: sunSvg,
  moon: moonSvg,
  earth: earthSvg,
  power: powerSvg,
  lock: lockSvg,
  user: userSvg,
  clear: clearSvg,
  retry: retrySvg,
  remove: removeSvg,
  pencil: pencilSvg,
  download: downloadSvg,
  upload: uploadSvg,
  filter: filterSvg,
} satisfies Record<string, string>;

export type IconName = keyof typeof sources;

// The files are trusted static assets; keep only the markup inside the root <svg>.
const inner = (svg: string) => svg.slice(svg.indexOf(">") + 1, svg.lastIndexOf("</svg>"));

export interface IconProps {
  name: IconName;
  /** Pixel size; Pajamas uses 16 (default), 12/14 in dense UI, 24 for empty states. */
  size?: 12 | 14 | 16 | 24;
  class?: string;
  /** Accessible name for a meaningful icon. Without it the icon is decorative (aria-hidden). */
  label?: string;
}

/** A 16px-grid Pajamas icon in `currentColor`. */
export function Icon(props: IconProps) {
  const [local] = splitProps(props, ["name", "size", "class", "label"]);
  return (
    <svg
      viewBox="0 0 16 16"
      width={local.size ?? 16}
      height={local.size ?? 16}
      fill="currentColor"
      class={`shrink-0 ${local.class ?? ""}`}
      role={local.label ? "img" : undefined}
      aria-label={local.label}
      aria-hidden={local.label ? undefined : "true"}
      innerHTML={inner(sources[local.name])}
    />
  );
}
