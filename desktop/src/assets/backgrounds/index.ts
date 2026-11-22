import auroraAsset from "./aurora.png";
import dunesAsset from "./dunes.png";
import studioAsset from "./studio.png";

// Vite types a PNG import as a string URL, but the webapp's root type-check
// also reaches this file (through tests/desktop-*) where Next's ambient
// `*.png` declaration types it as StaticImageData. Which wildcard wins depends
// on program file order, so normalize here instead of relying on it.
const assetUrl = (asset: string | { src: string }): string =>
  typeof asset === "string" ? asset : asset.src;

export const auroraBackground = assetUrl(auroraAsset);
export const dunesBackground = assetUrl(dunesAsset);
export const studioBackground = assetUrl(studioAsset);
