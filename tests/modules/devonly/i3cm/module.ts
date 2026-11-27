import type { ProfileDef } from "@i3cm/lib";

/** Kernel test module (not shipped): only for development tenants. */
export const module = { name: "devonly", requires: ["base"], developmentOnly: true };
export const rooms: string[] = [];
export const profiles: ProfileDef[] = [];
