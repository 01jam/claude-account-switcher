/** What `@rollup/plugin-yaml` hands back: the parsed document, shape unchecked.
 *  `src/i18n.ts` is the only place that reads it, and flattens it from there. */
declare module "*.yml" {
  const document: unknown;
  export default document;
}
