/* tslint:disable */
/* eslint-disable */

export class Canon {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Configuration for the page agent's `capture()`.
     */
    static agentConfig(): string;
    /**
     * Lay the unreduced copy out. Returns how many nodes it could not place exactly
     * (those become their own reference).
     */
    calibrate(render: Function): number;
    /**
     * Check the document last returned by `emit`, as laid out by the browser: `measure` is
     * the agent's `measure()` of it, `styles` its `styles()`. Returns JSON
     * `{structure, geometry: [...], styles: [...]}`.
     */
    check(measure: string, styles: string): string;
    /**
     * The finished markup. `style`: "auto" | "inline" | "classes". Returns JSON:
     * `{document, html, css, fontCss, elements, declarations}`. `hoist` / `fold` select the
     * CSS savings to apply (the caller verifies and falls back).
     */
    emit(style: string, hoist: boolean, fold: boolean, class_prefix: string): string;
    /**
     * Kinds of kept element to test per-tag inheritance on.
     */
    followClasses(): string;
    /**
     * `@font-face` rules for those faces. `fetched`: JSON `{url: base64}` of the files that
     * could be read; the others stay referenced by URL. Returns JSON `{css, faces}`.
     */
    static fontCss(sheets: string, loaded: string, fetched: string): string;
    /**
     * Font files to fetch for the faces the page actually used. `sheets`: JSON
     * `[[cssText, baseUrl]]` of the page's `@font-face` rules; `loaded`: the agent's
     * `loadedFonts()`. Returns a JSON array of URLs.
     */
    static fontUrls(sheets: string, loaded: string): string;
    /**
     * Snapshot info (viewport, rect, backdrop, flags...) as JSON.
     */
    info(): string;
    inheritTests(): string;
    /**
     * Load a snapshot (the page agent's `capture()` result) with options (JSON, may be "{}").
     */
    constructor(snapshot: string, options: string);
    prepare(): void;
    probeKeys(): string;
    /**
     * Captured property names, for the agent's `init()` in the rendering frame.
     */
    props(): string;
    /**
     * Remove every element the renderer cannot tell apart. `deadline`: a `Date.now()` value
     * to stop at (0 for none); what is verified by then is kept.
     */
    reduce(render: Function, deadline: number): void;
    /**
     * Statistics, warnings and counts, as JSON.
     */
    report(): string;
    setFollow(json: string): void;
    /**
     * `@font-face` rules the finished document carries (call before `emit`, not before
     * `reduce`: trial documents get their fonts from the frame's FontFaceSet).
     */
    setFontCss(css: string): void;
    setInherit(json: string): void;
    setProbe(json: string): void;
    /**
     * Where the copy's root should be placed in the rendering frame, `[x, y]` px, and the
     * stage width (or null).
     */
    stage(): string;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_canon_free: (a: number, b: number) => void;
    readonly canon_agentConfig: () => [number, number];
    readonly canon_calibrate: (a: number, b: any) => [number, number, number];
    readonly canon_check: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly canon_emit: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly canon_followClasses: (a: number) => [number, number];
    readonly canon_fontCss: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly canon_fontUrls: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly canon_info: (a: number) => [number, number];
    readonly canon_inheritTests: (a: number) => [number, number];
    readonly canon_new: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly canon_prepare: (a: number) => [number, number];
    readonly canon_probeKeys: (a: number) => [number, number];
    readonly canon_props: (a: number) => [number, number];
    readonly canon_reduce: (a: number, b: any, c: number) => [number, number];
    readonly canon_report: (a: number) => [number, number];
    readonly canon_setFollow: (a: number, b: number, c: number) => [number, number];
    readonly canon_setFontCss: (a: number, b: number, c: number) => void;
    readonly canon_setInherit: (a: number, b: number, c: number) => [number, number];
    readonly canon_setProbe: (a: number, b: number, c: number) => [number, number];
    readonly canon_stage: (a: number) => [number, number];
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
