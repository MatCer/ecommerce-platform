import { getViteConfig } from "astro/config";

export default getViteConfig({ test: { name: "theme-conversion" } }, { configFile: false });
