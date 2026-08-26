import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const brandDir = resolve(scriptDir, "..");
const assetsDir = join(brandDir, "assets");
const iconDir = join(brandDir, "app-icon");
const iconsetDir = join(iconDir, "AuraCoder.iconset");

mkdirSync(assetsDir, { recursive: true });
mkdirSync(iconDir, { recursive: true });

const fontCandidates = [
  join(homedir(), "Library/Fonts/Proxima Nova Alt Bold.otf"),
  join(homedir(), "Library/Fonts/Proxima Nova Bold.otf")
];
const fontPath = fontCandidates.find(existsSync);

if (!fontPath) {
  throw new Error("Proxima Nova Alt Bold was not found in ~/Library/Fonts");
}

const markSvg = ({ ink, accent }) => `<?xml version="1.0" encoding="UTF-8"?>
<svg viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
  <rect x="8" y="8" width="48" height="48" rx="12" stroke="${ink}" stroke-width="4"/>
  <path d="M26 10V54M28 27H54" stroke="${ink}" stroke-width="4" stroke-linecap="round"/>
  <rect x="34" y="34" width="14" height="14" rx="5" fill="${accent}"/>
</svg>
`;

const marks = {
  "auracoder-mark-on-dark.svg": { ink: "#F3F3F5", accent: "#61D596" },
  "auracoder-mark-on-light.svg": { ink: "#18181C", accent: "#02955A" },
  "auracoder-mark-mono-light.svg": { ink: "#FFFFFF", accent: "#FFFFFF" },
  "auracoder-mark-mono-dark.svg": { ink: "#000000", accent: "#000000" }
};

for (const [fileName, colors] of Object.entries(marks)) {
  writeFileSync(join(assetsDir, fileName), markSvg(colors));
}

writeFileSync(join(assetsDir, "auracoder-symbolic.svg"), `<?xml version="1.0" encoding="UTF-8"?>
<svg viewBox="0 0 64 64" fill="none" xmlns="http://www.w3.org/2000/svg">
  <rect x="8" y="8" width="48" height="48" rx="12" stroke="currentColor" stroke-width="4"/>
  <path d="M26 10V54M28 27H54" stroke="currentColor" stroke-width="4" stroke-linecap="round"/>
  <rect x="34" y="34" width="14" height="14" rx="5" fill="currentColor"/>
</svg>
`);

const wordmarkVariants = [
  ["auracoder-wordmark-on-dark.svg", "F3F3F5"],
  ["auracoder-wordmark-on-light.svg", "18181C"]
];

for (const [fileName, foreground] of wordmarkVariants) {
  execFileSync("hb-view", [
    "--output-format=svg",
    `--output-file=${join(assetsDir, fileName)}`,
    "--background=none",
    `--foreground=${foreground}`,
    "--font-size=256",
    "--margin=0",
    fontPath,
    "auracoder"
  ]);
}

const lockupSvg = ({ markFile, wordmarkFile }) => {
  const mark = readFileSync(join(assetsDir, markFile), "utf8");
  const wordmark = readFileSync(join(assetsDir, wordmarkFile), "utf8");
  const svgBody = (source) => {
    const svgStart = source.indexOf("<svg");
    const openTagEnd = source.indexOf(">", svgStart);
    return source.slice(openTagEnd + 1, source.lastIndexOf("</svg>"));
  };
  const markBody = svgBody(mark);
  const wordmarkBody = svgBody(wordmark);

  return `<?xml version="1.0" encoding="UTF-8"?>
<svg viewBox="0 0 276 64" fill="none" xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
  <svg x="0" y="0" width="64" height="64" viewBox="0 0 64 64">${markBody}</svg>
  <svg x="84" y="-18" width="182" height="80" viewBox="0 0 711 311.796875">${wordmarkBody}</svg>
</svg>
`;
};

writeFileSync(join(assetsDir, "auracoder-lockup-on-dark.svg"), lockupSvg({
  markFile: "auracoder-mark-on-dark.svg",
  wordmarkFile: "auracoder-wordmark-on-dark.svg"
}));
writeFileSync(join(assetsDir, "auracoder-lockup-on-light.svg"), lockupSvg({
  markFile: "auracoder-mark-on-light.svg",
  wordmarkFile: "auracoder-wordmark-on-light.svg"
}));

const sourceIcon = join(assetsDir, "app-icon-source.svg");
const renderPng = (size, output) => execFileSync("rsvg-convert", [
  "-w", String(size), "-h", String(size), sourceIcon, "-o", output
]);

for (const size of [16, 32, 48, 64, 128, 256, 512, 1024]) {
  renderPng(size, join(iconDir, `auracoder-${size}.png`));
}

rmSync(iconsetDir, { recursive: true, force: true });
mkdirSync(iconsetDir, { recursive: true });
const iconsetFiles = {
  "icon_16x16.png": 16,
  "icon_16x16@2x.png": 32,
  "icon_32x32.png": 32,
  "icon_32x32@2x.png": 64,
  "icon_128x128.png": 128,
  "icon_128x128@2x.png": 256,
  "icon_256x256.png": 256,
  "icon_256x256@2x.png": 512,
  "icon_512x512.png": 512,
  "icon_512x512@2x.png": 1024
};

for (const [fileName, size] of Object.entries(iconsetFiles)) {
  renderPng(size, join(iconsetDir, fileName));
}

execFileSync("iconutil", ["-c", "icns", iconsetDir, "-o", join(iconDir, "auracoder.icns")]);
execFileSync("magick", [
  join(iconDir, "auracoder-16.png"),
  join(iconDir, "auracoder-32.png"),
  join(iconDir, "auracoder-48.png"),
  join(iconDir, "auracoder-64.png"),
  join(iconDir, "auracoder-128.png"),
  join(iconDir, "auracoder-256.png"),
  join(iconDir, "auracoder.ico")
]);

console.log(`Built AuraCoder brand assets in ${brandDir}`);
