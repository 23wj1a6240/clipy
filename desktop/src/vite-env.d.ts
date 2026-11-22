/// <reference types="vite/client" />

/** Build-time edition, set by `define` in vite.config.ts: `"local"` in
 *  `vite --mode oss`, `"cloud"` otherwise. Read it through `lib/edition.ts`. */
declare const __CLIPY_EDITION__: "cloud" | "local";
/** `__CLIPY_EDITION__ === "local"`, as a literal so dead branches fold away. */
declare const __CLIPY_LOCAL_EDITION__: boolean;
