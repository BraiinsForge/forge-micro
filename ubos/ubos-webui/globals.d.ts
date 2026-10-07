/// <reference types="@rsbuild/core/types" />

// Build-time constants injected by Rsbuild's `source.define` (see
// rsbuild.config.ts). Rsbuild replaces each with a string literal at
// bundle time, so they never exist at runtime as real bindings; these
// declarations only tell the type checker they are in scope.
declare const __BRAND_NAME__: string;

// web/icons.svg is imported `?raw` (app.tsx) and handled by the Rspack
// asset/source rule in rsbuild.config.ts, which yields its contents as a
// string.
declare module '*.svg?raw' {
  const content: string;
  export default content;
}
