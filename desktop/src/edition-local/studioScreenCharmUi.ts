/**
 * Local edition stand-in for `lib/studioScreenCharmUi`, which re-exports from
 * the Studio animation editor. Only the retired Studio editor uses
 * it, and that is stubbed too, so this exports nothing: anything that imports
 * a name from it in the local build fails the build instead of pulling the
 * Studio code back in.
 */
export {};
