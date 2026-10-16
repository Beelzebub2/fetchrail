export type AccentSpot = { x: number; v: number };
export declare const PRESETS: Record<string, string>;
export declare function spotOf(text: string, theme: string, fallbackX?: number): AccentSpot | null;
export declare function accentSpot(accent: string): AccentSpot;
export declare function accentHex(spot: AccentSpot): string;
export declare function accentColors(spot: AccentSpot, theme: string): Record<string, string>;
export declare function accentField(theme: string): string;
export declare function distance(first: string, second: string): number;
