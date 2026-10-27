// Types the plugins share. Their `import type`s are erased before a host runs
// them, so this file never ships beside them.

/** Every key of every member of `U`, each optional: one allowlist reads them all. */
export type Fields<U> = {
  [K in U extends unknown ? keyof U : never]?: U extends unknown
    ? K extends keyof U
      ? U[K]
      : never
    : never
}
