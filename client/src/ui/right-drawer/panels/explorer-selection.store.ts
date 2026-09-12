/**
 * F7-3: moved to `core/explorer-selection.store.ts`. Re-exported from here so
 * importers that still point at this path - `views/chat/parts/text-part.ts`
 * and its spec, both out of scope for this change - keep working unchanged.
 * New code should import from `core/explorer-selection.store` directly.
 */
export {
  ExplorerSelectionStore,
  type PreviewRequest,
  type ExplorerOpenRequest,
} from '../../../core/explorer-selection.store';
