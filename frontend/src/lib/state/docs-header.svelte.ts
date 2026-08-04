/** Stamp chip shown in the docs top bar; set by the version layout,
 * cleared on leave. URL-derived state everywhere else — this exists only
 * because the top bar lives one layout above the version segment. */
class DocsHeaderState {
  stamp = $state<string | null>(null);
}

export const docsHeader = new DocsHeaderState();
