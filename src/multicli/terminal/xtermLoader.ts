/**
 * xterm is ~330 KB of JS plus CSS. It loads on the first terminal mount and the
 * module-level promise makes every later tab free. The CSS import lives here so
 * a build that never opens a terminal never ships the stylesheet.
 */

import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import type { WebglAddon } from "@xterm/addon-webgl";

export interface XtermModules {
  Terminal: typeof Terminal;
  FitAddon: typeof FitAddon;
  WebglAddon: typeof WebglAddon;
}

let xtermPromise: Promise<XtermModules> | null = null;

export function loadXterm(): Promise<XtermModules> {
  return (xtermPromise ??= Promise.all([
    import("@xterm/xterm"),
    import("@xterm/addon-fit"),
    import("@xterm/addon-webgl"),
    import("@xterm/xterm/css/xterm.css"),
  ]).then(([xterm, fit, webgl]) => ({
    Terminal: xterm.Terminal,
    FitAddon: fit.FitAddon,
    WebglAddon: webgl.WebglAddon,
  })));
}
