// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (components/screenshot-editor/useSceneHistory.ts) by the export; the desktop app shares it.
"use client";

import { useCallback, useMemo, useState } from "react";
import type { AnnotationScene } from "../../lib/screenshots/types";

const MAX_HISTORY = 60;
/** Repeats of the same coalesced change within this window share one undo step. */
const COALESCE_MS = 700;

interface HistoryState {
  past: AnnotationScene[];
  present: AnnotationScene;
  future: AnnotationScene[];
  /**
   * The scene as last persisted. Held in state (not a ref) so `dirty` is a
   * pure comparison: undoing back to the saved scene reads as clean again.
   */
  saved: AnnotationScene;
  /**
   * The last coalesced change and when it landed. Any other change, undo or
   * redo clears it, so a coalesced edit never folds into a step from before.
   */
  coalesce: { key: string; at: number } | null;
}

export interface CommitOptions {
  /**
   * Changes with the same key in quick succession (a color picker drag, a
   * held arrow key, a slider) become one undo step instead of one per event.
   */
  coalesceKey?: string;
}

export interface SceneHistory {
  scene: AnnotationScene;
  /** Live update with no undo entry — used while a drag is in flight. */
  setScene: (next: AnnotationScene | ((current: AnnotationScene) => AnnotationScene)) => void;
  /** Records the current scene as an undo point, then applies the change. */
  commit: (
    next: AnnotationScene | ((current: AnnotationScene) => AnnotationScene),
    options?: CommitOptions
  ) => void;
  /**
   * Marks an undo point without changing anything. Call once at the start of a
   * gesture whose intermediate frames are applied with `setScene`.
   */
  mark: () => void;
  undo: () => void;
  redo: () => void;
  canUndo: boolean;
  canRedo: boolean;
  dirty: boolean;
  markSaved: () => void;
}

function resolve(
  next: AnnotationScene | ((current: AnnotationScene) => AnnotationScene),
  current: AnnotationScene
): AnnotationScene {
  return typeof next === "function" ? next(current) : next;
}

export function useSceneHistory(initial: AnnotationScene): SceneHistory {
  const [state, setState] = useState<HistoryState>({
    past: [],
    present: initial,
    future: [],
    saved: initial,
    coalesce: null,
  });

  const setScene = useCallback<SceneHistory["setScene"]>((next) => {
    setState((current) => {
      const present = resolve(next, current.present);
      return present === current.present ? current : { ...current, present };
    });
  }, []);

  const mark = useCallback(() => {
    setState((current) => ({
      ...current,
      past: [...current.past, current.present].slice(-MAX_HISTORY),
      future: [],
      coalesce: null,
    }));
  }, []);

  const commit = useCallback<SceneHistory["commit"]>((next, options) => {
    const now = Date.now();
    setState((current) => {
      const present = resolve(next, current.present);
      if (present === current.present) return current;
      const key = options?.coalesceKey;
      const coalesce = key ? { key, at: now } : null;
      if (key && current.coalesce?.key === key && now - current.coalesce.at < COALESCE_MS) {
        return { ...current, present, future: [], coalesce };
      }
      return {
        ...current,
        past: [...current.past, current.present].slice(-MAX_HISTORY),
        present,
        future: [],
        coalesce,
      };
    });
  }, []);

  const undo = useCallback(() => {
    setState((current) => {
      if (current.past.length === 0) return current;
      return {
        ...current,
        past: current.past.slice(0, -1),
        present: current.past[current.past.length - 1],
        future: [current.present, ...current.future].slice(0, MAX_HISTORY),
        coalesce: null,
      };
    });
  }, []);

  const redo = useCallback(() => {
    setState((current) => {
      if (current.future.length === 0) return current;
      return {
        ...current,
        past: [...current.past, current.present].slice(-MAX_HISTORY),
        present: current.future[0],
        future: current.future.slice(1),
        coalesce: null,
      };
    });
  }, []);

  const markSaved = useCallback(() => {
    setState((current) => ({ ...current, saved: current.present }));
  }, []);

  return useMemo(
    () => ({
      scene: state.present,
      setScene,
      commit,
      mark,
      undo,
      redo,
      canUndo: state.past.length > 0,
      canRedo: state.future.length > 0,
      dirty: state.present !== state.saved,
      markSaved,
    }),
    [commit, mark, markSaved, redo, setScene, state, undo]
  );
}
