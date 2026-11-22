import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { LazyStore } from "@tauri-apps/plugin-store";
import {
  clearClipboardHistory,
  copyClipboardEntry,
  deleteClipboardEntry,
  editClipboardEntry,
  getClipboardHistory,
  getRecorderShortcuts,
  hideClipboardHistory,
  onClipboardHistoryChanged,
  onClipboardHistoryOpened,
  pasteClipboardEntry,
  readClipboardImage,
  readClipboardImageThumbnail,
  setClipboardEntryFavorite,
  type ClipEntryDto,
  type ClipImageDto,
} from "../lib/bridge";
import { Icon, type IconName } from "../components/recorder/ui";
import { clipDetail, clipFirstLine, clipKind, clipKindLabel, clipSwatch, type ClipKind } from "../lib/clipKind";
import { formatHotkey } from "../lib/hotkey";
import { useDesktopT, type DesktopT } from "../lib/messages";
import { deviceName, isMacOS } from "../lib/platform";
import { followSystemLook } from "../lib/recorderLook";

const MOD = isMacOS ? "⌘" : "Ctrl+";
const settingsStore = new LazyStore("settings.json");
/** Set once the user has seen where the picker's key can be changed. */
const HINT_SEEN_KEY = "clipboard_history_hint_seen";

/** How long ago, as the `clipboard.elapsed*` messages take it. */
type Elapsed = { unit: "now" | "m" | "h" | "d"; count: number };

function ago(ms: number): Elapsed {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return { unit: "now", count: 0 };
  if (s < 3600) return { unit: "m", count: Math.floor(s / 60) };
  if (s < 86400) return { unit: "h", count: Math.floor(s / 3600) };
  return { unit: "d", count: Math.floor(s / 86400) };
}

/** ClipBook's label for an image item, also what search matches. */
function imageLabel(image: ClipImageDto, t: DesktopT): string {
  return t("clipboard.imageLabel", { width: String(image.width), height: String(image.height) });
}

function searchText(entry: ClipEntryDto, t: DesktopT): string {
  if (!entry.image) return entry.text;
  // "1920x1080" finds it too, since that is what people type.
  const { width, height } = entry.image;
  return `${imageLabel(entry.image, t)} ${width}x${height} ${entry.text}`;
}

/** One object URL per picture, shared by every row that shows it. */
const thumbnails = new Map<string, Promise<string>>();

function thumbnailUrl(id: number, hash: string): Promise<string> {
  let url = thumbnails.get(hash);
  if (!url) {
    url = readClipboardImageThumbnail(id).then((bytes) =>
      URL.createObjectURL(new Blob([bytes], { type: "image/png" })),
    );
    // A failed read is tried again next time instead of staying blank.
    url.catch(() => thumbnails.delete(hash));
    thumbnails.set(hash, url);
  }
  return url;
}

/** Frees the previews of pictures that left the history. */
function forgetThumbnails(entries: ClipEntryDto[]) {
  const kept = new Set(entries.map((entry) => entry.image?.hash).filter(Boolean));
  for (const [hash, url] of thumbnails) {
    if (kept.has(hash)) continue;
    thumbnails.delete(hash);
    void url.then(URL.revokeObjectURL, () => {});
  }
}

/** The thumbnail for one picture. The URL is kept with the hash it belongs
 *  to, so a component that switches pictures (the preview pane) never shows
 *  the previous one while the next loads. */
function useThumbnail(id: number, hash: string): string | null {
  const [thumb, setThumb] = useState<{ hash: string; url: string } | null>(null);
  useEffect(() => {
    let live = true;
    thumbnailUrl(id, hash)
      .then((url) => live && setThumb({ hash, url }))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [id, hash]);
  return thumb?.hash === hash ? thumb.url : null;
}

/** How long a selection must stay on an image before its full picture is
 *  read. Sweeping the pointer across the grid selects every card it passes,
 *  and a read cannot be cancelled once started. */
const FULL_IMAGE_DELAY_MS = 150;

/** History can hold 10,000 items, and rendering every row made each hover
 *  and arrow key re-render all of them. The list is built in pages: the
 *  first page on open, the next when the list is scrolled near its end or
 *  the selection comes within `RENDER_AHEAD` of the last row built. Search
 *  and the tabs still cover the whole history. */
export const RENDER_PAGE = 200;
const RENDER_AHEAD = 50;

/** The whole picture for the preview pane. Only the selected item's is kept,
 *  so a history of large screenshots never sits in memory at once. Shows
 *  the thumbnail until it arrives. */
function useFullImage(id: number, hash: string): string | null {
  const thumbnail = useThumbnail(id, hash);
  const [full, setFull] = useState<{ hash: string; url: string } | null>(null);
  useEffect(() => {
    let live = true;
    let url: string | null = null;
    const timer = window.setTimeout(() => {
      readClipboardImage(id)
        .then((bytes) => {
          if (!live) return;
          url = URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
          setFull({ hash, url });
        })
        .catch(() => {});
    }, FULL_IMAGE_DELAY_MS);
    return () => {
      live = false;
      window.clearTimeout(timer);
      if (url) {
        URL.revokeObjectURL(url);
        // Drop the revoked URL too, or coming back to this picture would
        // show it (a broken image) until the next read lands.
        setFull(null);
      }
    };
  }, [id, hash]);
  return full?.hash === hash ? full.url : thumbnail;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const KIND_ICON: Record<Exclude<ClipKind, "image" | "color">, IconName> = {
  link: "link",
  email: "mail",
  code: "code",
  text: "lines",
};

/** The coloured square at the start of a text row: a swatch for a copied
 *  colour, else an icon for the kind of text. Images are grid cards. */
function ClipLead({ entry, kind }: { entry: ClipEntryDto; kind: ClipKind }) {
  const swatch = clipSwatch(entry);
  if (swatch) return <span className="clip-swatch h-[30px] w-[30px] shrink-0 rounded-[8px]" style={{ background: swatch }} />;
  if (kind === "image" || kind === "color") return null;
  return (
    <span className="clip-tile flex h-[30px] w-[30px] shrink-0 items-center justify-center rounded-[8px]">
      <Icon name={KIND_ICON[kind]} size={16} />
    </span>
  );
}

type Filter = "all" | "text" | "images" | "favorites";

const FILTERS: Filter[] = ["all", "text", "images", "favorites"];

function matchesFilter(entry: ClipEntryDto, filter: Filter): boolean {
  if (filter === "text") return !entry.image;
  if (filter === "images") return Boolean(entry.image);
  if (filter === "favorites") return Boolean(entry.favorite);
  return true;
}

function IconButton({
  icon,
  label,
  onClick,
  disabled,
  active,
}: {
  icon: IconName;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
}) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className={`flex h-6 w-6 shrink-0 items-center justify-center rounded-md hover:bg-row disabled:opacity-40 disabled:hover:bg-transparent ${
        active ? "text-spark-500" : "text-ink-secondary"
      }`}
    >
      <Icon name={icon} size={15} />
    </button>
  );
}

type MenuState = { entry: ClipEntryDto; x: number; y: number };

type ArrowKey = "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight";

/**
 * The item an arrow key moves to. Text rows span the list and images sit in
 * a grid between them, so up and down go by what is on screen: the nearest
 * line above or below, and on it the item under the current one. Left and
 * right step through the list in order. Without layout (no sizes measured),
 * every arrow steps by one.
 */
function arrowTarget(list: HTMLElement | null, from: number, key: ArrowKey, count: number): number {
  const step = key === "ArrowDown" || key === "ArrowRight" ? 1 : -1;
  const next = Math.max(0, Math.min(count - 1, from + step));
  if (key === "ArrowLeft" || key === "ArrowRight" || !list) return next;
  const items = [...list.querySelectorAll<HTMLElement>("[data-index]")].map((el) => ({
    index: Number(el.dataset.index),
    rect: el.getBoundingClientRect(),
    card: el.classList.contains("clip-card"),
  }));
  const current = items.find((item) => item.index === from);
  if (!current || current.rect.height === 0) return next;
  const { rect } = current;
  // From a card, stay in its column; from a full-width row, start a grid
  // line at its first card.
  const x = current.card ? rect.left + rect.width / 2 : rect.left + 1;
  const down = key === "ArrowDown";
  const candidates = items
    .filter((item) => (down ? item.rect.top >= rect.bottom - 1 : item.rect.bottom <= rect.top + 1))
    .map((item) => ({
      index: item.index,
      gap: down ? item.rect.top - rect.bottom : rect.top - item.rect.bottom,
      across: Math.max(0, item.rect.left - x, x - item.rect.right),
    }));
  if (candidates.length === 0) return from;
  const nearest = Math.min(...candidates.map((c) => c.gap));
  const line = candidates.filter((c) => c.gap - nearest < 2);
  line.sort((a, b) => a.across - b.across || a.index - b.index);
  return line[0].index;
}

/** The right-hand pane: the selected item in full, with what it is and the
 *  same Paste and Copy the footer offers. */
function ClipPreview({
  entry,
  onPaste,
  onCopy,
  onEdit,
}: {
  entry: ClipEntryDto | undefined;
  onPaste: () => void;
  onCopy: () => void;
  onEdit: () => void;
}) {
  const t = useDesktopT();
  if (!entry) {
    return (
      <aside className="clip-preview flex w-[36%] min-w-[280px] max-w-[460px] shrink-0 items-center justify-center border-l border-subtle px-6 text-center text-[13px] text-ink-muted">
        {t("clipboard.preview.empty")}
      </aside>
    );
  }
  const kind = clipKind(entry);
  const swatch = clipSwatch(entry);
  return (
    <aside
      data-kind={kind}
      className="clip-preview clip-kind flex w-[36%] min-w-[280px] max-w-[460px] shrink-0 flex-col gap-3 border-l border-subtle p-4"
    >
      {entry.image ? (
        <PreviewImage id={entry.id} hash={entry.image.hash} />
      ) : (
        <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-hidden rounded-[10px] border border-subtle bg-row">
          {swatch && <span className="clip-swatch h-16 w-full shrink-0" style={{ background: swatch }} />}
          <pre
            className={`clipy-scroll min-h-0 flex-1 overflow-y-auto whitespace-pre-wrap break-words px-3 py-2.5 leading-[1.5] text-ink-text ${
              kind === "code" || kind === "color" ? "font-mono text-[12.5px]" : "font-sans text-[13px]"
            }`}
          >
            {entry.text}
          </pre>
        </div>
      )}
      <div className="grid gap-1">
        <p className="truncate text-[14px] font-semibold text-ink-text">
          {entry.image ? imageLabel(entry.image, t) : clipFirstLine(entry.text)}
        </p>
        <p className="truncate text-[12px] text-ink-muted">
          <span className="clip-kind-label mr-1.5 font-bold">{clipKindLabel(kind, t)}</span>
          {entry.image ? formatBytes(entry.image.bytes) : clipDetail(entry, t)} · {t("clipboard.copiedAgo", ago(entry.copiedAt))}
          {entry.favorite ? ` · ${t("clipboard.preview.favorite")}` : ""}
        </p>
        {entry.image && entry.text.trim() && (
          <p className="line-clamp-2 text-[12px] text-ink-secondary">{entry.text.trim()}</p>
        )}
      </div>
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onPaste}
          className="rounded-md bg-flash-600 px-3 py-1 text-[12.5px] font-medium text-white"
        >
          {t("clipboard.preview.paste")}
        </button>
        <button
          type="button"
          onClick={onCopy}
          className="rounded-md bg-row px-3 py-1 text-[12.5px] font-medium text-ink-text hover:bg-row-hover"
        >
          {t("clipboard.preview.copy")}
        </button>
        {!entry.image && (
          <button
            type="button"
            onClick={onEdit}
            className="rounded-md bg-row px-3 py-1 text-[12.5px] font-medium text-ink-text hover:bg-row-hover"
          >
            {t("clipboard.preview.edit")}
          </button>
        )}
      </div>
    </aside>
  );
}

function PreviewImage({ id, hash }: { id: number; hash: string }) {
  const src = useFullImage(id, hash);
  return (
    <div className="flex min-h-[180px] flex-1 items-center justify-center overflow-hidden rounded-[10px] border border-subtle bg-row p-2">
      {src && <img src={src} alt="" draggable={false} className="max-h-full max-w-full object-contain" />}
    </div>
  );
}

/** Asks for a card's thumbnail only once the card is on screen or close to
 *  it, so opening a long history does not read (or, right after the
 *  thumbnail size grew, rebuild) every picture at once. Stays loaded after. */
function CardThumbnail({ id, hash }: { id: number; hash: string }) {
  const box = useRef<HTMLSpanElement>(null);
  const [near, setNear] = useState(() => typeof IntersectionObserver === "undefined");
  useEffect(() => {
    const el = box.current;
    if (near || !el) return;
    const observer = new IntersectionObserver(
      (seen) => {
        if (seen.some((entry) => entry.isIntersecting)) setNear(true);
      },
      { root: el.closest("ul"), rootMargin: "400px 0px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [near]);
  return (
    <span ref={box} className="flex h-full w-full items-center justify-center">
      {near && <LoadedThumbnail id={id} hash={hash} />}
    </span>
  );
}

function LoadedThumbnail({ id, hash }: { id: number; hash: string }) {
  const src = useThumbnail(id, hash);
  return src ? <img src={src} alt="" draggable={false} className="max-h-full max-w-full object-contain" /> : null;
}

/**
 * The clipboard history picker: type to filter, arrows to move, Enter (or
 * ⌘1–9) pastes into the app underneath, ⌘C copies without pasting. Tab
 * switches between all, text, images and favorites; right-click (or the row
 * buttons) stars, edits and deletes. The window is reused, so every open
 * resets the search, filter and selection.
 */
export default function ClipboardHistory() {
  const t = useDesktopT();
  const [entries, setEntries] = useState<ClipEntryDto[]>([]);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [renderCount, setRenderCount] = useState(RENDER_PAGE);
  const [selected, setSelected] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [editing, setEditing] = useState<{ id: number; draft: string } | null>(null);
  // The key is taken from other apps (paste as plain text), so the first
  // open says it can be moved.
  const [hintKey, setHintKey] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    void settingsStore
      .get<boolean>(HINT_SEEN_KEY)
      .then(async (seen) => {
        if (seen || disposed) return;
        const rows = await getRecorderShortcuts();
        const hotkey = rows.find((row) => row.action === "clipboard_history")?.hotkey;
        if (!disposed && hotkey) setHintKey(formatHotkey(hotkey));
      })
      .catch(() => {});
    return () => {
      disposed = true;
    };
  }, []);

  const dismissHint = useCallback(() => {
    setHintKey(null);
    void settingsStore
      .set(HINT_SEEN_KEY, true)
      .then(() => settingsStore.save())
      .catch(() => {});
  }, []);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  // Light or dark with the system, like the rest of the OS's own panels.
  useEffect(() => followSystemLook(), []);

  const refresh = useCallback(() => {
    getClipboardHistory()
      .then((next) => {
        forgetThumbnails(next);
        setEntries(next);
      })
      .catch((err) => setError(String(err)));
  }, []);

  useEffect(() => {
    refresh();
    const offs = [
      onClipboardHistoryChanged(refresh),
      onClipboardHistoryOpened(() => {
        setQuery("");
        setFilter("all");
        setSelected(0);
        setRenderCount(RENDER_PAGE);
        setError(null);
        setMenu(null);
        setEditing(null);
        refresh();
        inputRef.current?.focus();
      }),
    ];
    return () => offs.forEach((off) => void off.then((fn) => fn()));
  }, [refresh]);

  // Favorites first, as ClipBook pins them; newest first within each group.
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    const shown = entries.filter(
      (e) => matchesFilter(e, filter) && (!q || searchText(e, t).toLowerCase().includes(q)),
    );
    return [...shown.filter((e) => e.favorite), ...shown.filter((e) => !e.favorite)];
  }, [entries, query, filter, t]);

  const rendered = useMemo(() => visible.slice(0, renderCount), [visible, renderCount]);
  const renderMore = useCallback(() => {
    setRenderCount((count) => (count < visible.length ? count + RENDER_PAGE : count));
  }, [visible.length]);

  useEffect(() => setSelected(0), [query, filter]);
  useEffect(() => setRenderCount(RENDER_PAGE), [query, filter]);
  useEffect(() => {
    if (selected >= renderCount - RENDER_AHEAD) renderMore();
  }, [selected, renderCount, renderMore]);
  useEffect(() => {
    setSelected((i) => Math.max(0, Math.min(i, visible.length - 1)));
  }, [visible.length]);
  useEffect(() => {
    listRef.current?.querySelector(`[data-index="${selected}"]`)?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  const counts = useMemo<Record<Filter, number>>(
    () => ({
      all: entries.length,
      text: entries.filter((e) => matchesFilter(e, "text")).length,
      images: entries.filter((e) => matchesFilter(e, "images")).length,
      favorites: entries.filter((e) => matchesFilter(e, "favorites")).length,
    }),
    [entries],
  );
  const favoriteCount = useMemo(() => visible.filter((e) => e.favorite).length, [visible]);
  // Favorites and the rest get their own headings, unless the list is only
  // favorites already.
  const grouped = filter !== "favorites" && favoriteCount > 0;

  const fail = useCallback((err: unknown) => setError(String(err)), []);

  const paste = useCallback((entry: ClipEntryDto | undefined) => {
    if (!entry) return;
    if (hintKey) dismissHint();
    // A paste that could not be completed is reported by a notification from
    // Rust; the picker is gone by then.
    pasteClipboardEntry(entry.id).catch(fail);
  }, [hintKey, dismissHint, fail]);

  const copy = useCallback((entry: ClipEntryDto | undefined) => {
    if (!entry) return;
    copyClipboardEntry(entry.id).catch(fail);
  }, [fail]);

  const remove = useCallback(
    (entry: ClipEntryDto | undefined) => {
      if (!entry) return;
      deleteClipboardEntry(entry.id).then(refresh).catch(fail);
    },
    [refresh, fail],
  );

  const toggleFavorite = useCallback(
    (entry: ClipEntryDto | undefined) => {
      if (!entry) return;
      setClipboardEntryFavorite(entry.id, !entry.favorite).then(refresh).catch(fail);
    },
    [refresh, fail],
  );

  const startEdit = useCallback((entry: ClipEntryDto | undefined) => {
    if (!entry || entry.image) return;
    setMenu(null);
    setError(null);
    setEditing({ id: entry.id, draft: entry.text });
  }, []);

  const finishEdit = useCallback(
    (save: boolean) => {
      if (!editing) return;
      const done = () => {
        setEditing(null);
        inputRef.current?.focus();
      };
      if (!save) return done();
      editClipboardEntry(editing.id, editing.draft)
        .then(() => {
          setError(null);
          done();
          refresh();
        })
        .catch(fail);
    },
    [editing, refresh, fail],
  );

  const cycleFilter = (step: number) => {
    const at = FILTERS.indexOf(filter);
    setFilter(FILTERS[(at + step + FILTERS.length) % FILTERS.length]);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // The edit box and the menu handle their own keys.
    if (editing) return;
    const mod = isMacOS ? e.metaKey : e.ctrlKey;
    const current = visible[selected];
    if (e.key === "Escape") {
      e.preventDefault();
      if (menu) setMenu(null);
      else void hideClipboardHistory();
    } else if (
      e.key === "ArrowDown" ||
      e.key === "ArrowUp" ||
      // Left and right move the caret in the search box, except on a card,
      // where they move along the grid.
      ((e.key === "ArrowLeft" || e.key === "ArrowRight") && current?.image)
    ) {
      e.preventDefault();
      setSelected((i) => arrowTarget(listRef.current, i, e.key as ArrowKey, visible.length));
    } else if (e.key === "Tab") {
      e.preventDefault();
      cycleFilter(e.shiftKey ? -1 : 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      paste(current);
    } else if (mod && /^[1-9]$/.test(e.key)) {
      e.preventDefault();
      paste(visible[Number(e.key) - 1]);
    } else if (mod && e.key.toLowerCase() === "c") {
      // Text selected in the search box copies as usual.
      const input = inputRef.current;
      if (input && document.activeElement === input && input.selectionStart !== input.selectionEnd) return;
      e.preventDefault();
      copy(current);
    } else if (mod && e.key.toLowerCase() === "s") {
      e.preventDefault();
      toggleFavorite(current);
    } else if (mod && e.key.toLowerCase() === "e") {
      e.preventDefault();
      startEdit(current);
    } else if (mod && (e.key === "Backspace" || e.key === "Delete")) {
      e.preventDefault();
      remove(current);
    }
  };

  const current = visible[selected];
  const menuItems = menu
    ? [
        { label: t("clipboard.menu.paste"), keys: "↵", run: () => paste(menu.entry) },
        { label: t("clipboard.menu.copy"), keys: `${MOD}C`, run: () => copy(menu.entry) },
        {
          label: menu.entry.favorite ? t("clipboard.menu.removeFavorite") : t("clipboard.menu.addFavorite"),
          keys: `${MOD}S`,
          run: () => toggleFavorite(menu.entry),
        },
        ...(menu.entry.image
          ? []
          : [{ label: t("clipboard.menu.edit"), keys: `${MOD}E`, run: () => startEdit(menu.entry) }]),
        { label: t("clipboard.menu.delete"), keys: `${MOD}⌫`, run: () => remove(menu.entry), danger: true },
      ]
    : [];

  return (
    <main
      onKeyDown={onKeyDown}
      onClick={() => setMenu(null)}
      // Clicks keep the keyboard in the search box (or the edit box): a
      // click that moved focus to the page would leave every key, Esc
      // included, with nothing to handle it.
      onMouseDown={(e) => {
        const target = e.target;
        if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) return;
        e.preventDefault();
      }}
      className="clip-picker popover-shell relative flex !h-screen !w-screen select-none flex-col overflow-hidden text-ink-text"
    >
      <div aria-hidden className="clip-strip" />
      <header data-tauri-drag-region className="grid gap-2.5 px-3.5 pb-2.5 pt-3">
        <label className="clip-search flex h-[38px] items-center gap-2 rounded-[10px] px-2.5">
          <Icon name="search" size={16} className="clip-search-icon shrink-0" />
          <input
            ref={inputRef}
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("clipboard.search")}
            spellCheck={false}
            className="min-w-0 flex-1 bg-transparent text-[15px] text-ink-text placeholder:text-ink-muted focus:outline-none"
          />
          <IconButton icon="paste" label={t("clipboard.pasteLabel")} disabled={!current} onClick={() => paste(current)} />
          <IconButton
            icon="copy"
            label={t("clipboard.copyLabel", { keys: `${MOD}C` })}
            disabled={!current}
            onClick={() => copy(current)}
          />
        </label>

        <nav className="flex items-center gap-1.5" aria-label={t("clipboard.filterLabel")}>
          {FILTERS.map((f) => (
            <button
              key={f}
              type="button"
              aria-pressed={filter === f}
              onClick={() => {
                setFilter(f);
                inputRef.current?.focus();
              }}
              className="clip-tab inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[12.5px] font-semibold"
            >
              {t(`clipboard.filters.${f}`)}
              <span className="text-[11px] tabular-nums opacity-75">{counts[f]}</span>
            </button>
          ))}
          {entries.some((e) => !e.favorite) && (
            <button
              type="button"
              title={t("clipboard.clearAllTitle")}
              onClick={() => void clearClipboardHistory().then(refresh).catch(fail)}
              className="ml-auto rounded-md px-1.5 py-1 text-[12px] font-semibold text-ink-muted hover:bg-row hover:text-ink-text"
            >
              {t("clipboard.clearAll")}
            </button>
          )}
        </nav>
      </header>

      {hintKey && (
        <div className="flex items-start gap-2 border-y border-subtle bg-row px-4 py-2 text-[12px] leading-snug text-ink-secondary">
          <span className="min-w-0 flex-1">{t("clipboard.hint", { hotkey: hintKey })}</span>
          <button
            type="button"
            onClick={dismissHint}
            aria-label={t("clipboard.dismissHint")}
            className="h-5 w-5 shrink-0 rounded text-[14px] leading-none hover:bg-row-hover"
          >
            ×
          </button>
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        {/* Text rows span the list; images fill grid cells between them, in
            the same newest-first order. */}
        <ul
          ref={listRef}
          onScroll={(e) => {
            const list = e.currentTarget;
            if (list.scrollHeight - list.scrollTop - list.clientHeight < 800) renderMore();
          }}
          className="clipy-scroll grid min-h-0 flex-1 auto-rows-max grid-cols-[repeat(auto-fill,minmax(220px,1fr))] content-start gap-x-2.5 gap-y-1 overflow-y-auto px-3 pb-3 pt-1"
        >
          {rendered.map((entry, index) => {
            const heading =
              grouped && index === 0
                ? t("clipboard.headings.favorites")
                : grouped && index === favoriteCount
                  ? t("clipboard.headings.recent")
                  : null;
            const kind = clipKind(entry);
            const row =
              editing?.id === entry.id ? (
                <li key={entry.id} data-index={index} className="col-span-full rounded-[10px] bg-row px-3 py-2">
                  <textarea
                    autoFocus
                    value={editing.draft}
                    onChange={(e) => setEditing({ id: entry.id, draft: e.target.value })}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === "Escape") {
                        e.preventDefault();
                        finishEdit(false);
                      } else if (e.key === "Enter" && (isMacOS ? e.metaKey : e.ctrlKey)) {
                        e.preventDefault();
                        finishEdit(true);
                      }
                    }}
                    spellCheck={false}
                    rows={Math.min(8, Math.max(3, editing.draft.split("\n").length))}
                    className="w-full resize-none rounded-md border border-subtle bg-surface px-2 py-1.5 font-mono text-[12.5px] leading-[1.45] text-ink-text focus:outline-none"
                  />
                  <div className="mt-1.5 flex items-center gap-2 text-[11px] text-ink-muted">
                    <span>{t("clipboard.editHint", { keys: `${MOD}↵` })}</span>
                    <button
                      type="button"
                      onClick={() => finishEdit(false)}
                      className="ml-auto rounded-md px-2 py-0.5 text-[12px] font-medium text-ink-secondary hover:bg-row-hover"
                    >
                      {t("clipboard.cancel")}
                    </button>
                    <button
                      type="button"
                      onClick={() => finishEdit(true)}
                      className="rounded-md bg-flash-600 px-2.5 py-0.5 text-[12px] font-medium text-white"
                    >
                      {t("clipboard.save")}
                    </button>
                  </div>
                </li>
              ) : entry.image ? (
                <li
                  key={entry.id}
                  data-index={index}
                  data-kind={kind}
                  data-selected={index === selected}
                  onMouseMove={() => setSelected(index)}
                  onClick={() => paste(entry)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    setSelected(index);
                    setMenu({ entry, x: e.clientX, y: e.clientY });
                  }}
                  // Like the rows: a fixed size and an always-present border,
                  // so moving the selection never shifts the grid.
                  className="clip-row clip-card clip-kind group flex cursor-default flex-col gap-1.5 rounded-[12px] p-1.5"
                >
                  <span className="relative flex aspect-[16/10] w-full items-center justify-center overflow-hidden rounded-[8px] border border-subtle bg-row">
                    <CardThumbnail id={entry.id} hash={entry.image.hash} />
                    <span
                      className={`absolute right-1 top-1 flex items-center gap-0.5 rounded-md bg-surface/90 p-0.5 shadow-sm ${
                        index === selected ? "visible" : "invisible group-hover:visible"
                      }`}
                    >
                      <IconButton icon="trash" label={t("clipboard.rowDelete", { keys: `${MOD}⌫` })} onClick={() => remove(entry)} />
                      <IconButton
                        icon={entry.favorite ? "starFilled" : "star"}
                        label={
                        entry.favorite
                          ? t("clipboard.rowRemoveFavorite", { keys: `${MOD}S` })
                          : t("clipboard.rowAddFavorite", { keys: `${MOD}S` })
                      }
                        active={entry.favorite}
                        onClick={() => toggleFavorite(entry)}
                      />
                    </span>
                    {entry.favorite && index !== selected && (
                      <span className="absolute right-1.5 top-1.5 text-spark-500 group-hover:invisible">
                        <Icon name="starFilled" size={14} />
                      </span>
                    )}
                  </span>
                  <span className="flex min-w-0 items-center gap-2 px-1">
                    <span className="grid min-w-0 flex-1 gap-px">
                      <span className="truncate text-[12.5px] font-medium leading-[1.35]">{imageLabel(entry.image, t)}</span>
                      <span className="truncate text-[11.5px] text-ink-muted">
                        {entry.text.trim() ? clipFirstLine(entry.text) : t("clipboard.elapsedAgo", ago(entry.copiedAt))}
                      </span>
                    </span>
                    {index < 9 && (
                      <span className="clip-key min-w-[34px] shrink-0 rounded-md px-1.5 py-0.5 text-center font-mono text-[11px] tabular-nums">
                        {MOD}
                        {index + 1}
                      </span>
                    )}
                  </span>
                </li>
              ) : (
                <li
                  key={entry.id}
                  data-index={index}
                  data-kind={kind}
                  data-selected={index === selected}
                  onMouseMove={() => setSelected(index)}
                  onClick={() => paste(entry)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    setSelected(index);
                    setMenu({ entry, x: e.clientX, y: e.clientY });
                  }}
                  // A fixed height and an always-present border: moving the
                  // selection never changes a row's size, so nothing jumps.
                  className="clip-row clip-kind group col-span-full flex h-[52px] cursor-default items-center gap-2.5 rounded-[10px] px-2"
                >
                  <ClipLead entry={entry} kind={kind} />
                  <span className="grid min-w-0 flex-1 gap-px">
                    <span
                      className={`truncate leading-[1.35] ${
                        kind === "code" || kind === "color" ? "font-mono text-[12.5px]" : "text-[13px]"
                      }`}
                    >
                      {clipFirstLine(entry.text)}
                    </span>
                    <span className="truncate text-[11.5px] text-ink-muted">
                      <span className="clip-kind-label mr-1.5 font-bold">{clipKindLabel(kind, t)}</span>
                      {`${clipDetail(entry, t)} · ${t("clipboard.elapsedAgo", ago(entry.copiedAt))}`}
                    </span>
                  </span>
                  {/* Every row lays out the same buttons and only shows them when
                      selected or hovered, so the text never reflows either. */}
                  <span
                    className={`flex shrink-0 items-center gap-0.5 ${
                      index === selected ? "visible" : "invisible group-hover:visible"
                    }`}
                  >
                    <IconButton icon="edit" label={t("clipboard.rowEdit", { keys: `${MOD}E` })} onClick={() => startEdit(entry)} />
                    <IconButton icon="trash" label={t("clipboard.rowDelete", { keys: `${MOD}⌫` })} onClick={() => remove(entry)} />
                  </span>
                  <span className={entry.favorite || index === selected ? "visible" : "invisible group-hover:visible"}>
                    <IconButton
                      icon={entry.favorite ? "starFilled" : "star"}
                      label={
                        entry.favorite
                          ? t("clipboard.rowRemoveFavorite", { keys: `${MOD}S` })
                          : t("clipboard.rowAddFavorite", { keys: `${MOD}S` })
                      }
                      active={entry.favorite}
                      onClick={() => toggleFavorite(entry)}
                    />
                  </span>
                  {index < 9 ? (
                    <span className="clip-key min-w-[34px] shrink-0 rounded-md px-1.5 py-0.5 text-center font-mono text-[11px] tabular-nums">
                      {MOD}
                      {index + 1}
                    </span>
                  ) : (
                    <span className="min-w-[34px] shrink-0 text-center text-[11px] tabular-nums text-ink-muted">
                      {t("clipboard.elapsed", ago(entry.copiedAt))}
                    </span>
                  )}
                </li>
              );
            return heading ? (
              <Fragment key={entry.id}>
                <li
                  aria-hidden
                  className="col-span-full px-2 pb-1 pt-2 text-[11px] font-bold uppercase tracking-[0.08em] text-ink-muted"
                >
                  {heading}
                </li>
                {row}
              </Fragment>
            ) : (
              row
            );
          })}
          {visible.length === 0 && (
            <li className="col-span-full px-3 py-10 text-center text-[13px] text-ink-secondary">
              {entries.length === 0
                ? t("clipboard.empty.nothing")
                : filter === "favorites" && !query.trim()
                  ? t("clipboard.empty.noFavorites", { keys: `${MOD}S` })
                  : t("clipboard.empty.noMatches")}
            </li>
          )}
        </ul>
        <ClipPreview
          entry={current}
          onPaste={() => paste(current)}
          onCopy={() => copy(current)}
          onEdit={() => startEdit(current)}
        />
      </div>

      {menu && (
        <div
          role="menu"
          onClick={(e) => e.stopPropagation()}
          style={{ left: Math.min(menu.x, window.innerWidth - 216), top: Math.min(menu.y, window.innerHeight - 190) }}
          className="absolute z-10 w-[208px] rounded-lg border border-subtle bg-surface py-1 shadow-lg"
        >
          {menuItems.map((item) => (
            <button
              key={item.label}
              type="button"
              role="menuitem"
              onClick={() => {
                setMenu(null);
                item.run();
              }}
              className={`flex w-full items-center px-3 py-1.5 text-left text-[12.5px] hover:bg-row ${
                "danger" in item && item.danger ? "text-status-error" : "text-ink-text"
              }`}
            >
              {item.label}
              <span className="ml-auto text-[11px] text-ink-muted">{item.keys}</span>
            </button>
          ))}
        </div>
      )}

      <footer className="clip-foot flex items-center gap-0.5 whitespace-nowrap border-t border-subtle px-2 py-1.5 text-[11.5px] text-ink-muted">
        <FooterAction keys="↵" label={t("clipboard.footer.paste")} disabled={!current} onClick={() => paste(current)} />
        <FooterAction keys={`${MOD}C`} label={t("clipboard.footer.copy")} disabled={!current} onClick={() => copy(current)} />
        <FooterAction
          keys={`${MOD}S`}
          label={t("clipboard.footer.star")}
          disabled={!current}
          onClick={() => toggleFavorite(current)}
        />
        <FooterAction keys="⇥" label={t("clipboard.footer.type")} onClick={() => cycleFilter(1)} />
        {/* Ctrl+C and Ctrl+S are wider than their ⌘ keys, and the footer
            has no room for all five and the note; Esc closes it anyway. */}
        {isMacOS && (
          <FooterAction keys="esc" label={t("clipboard.footer.close")} onClick={() => void hideClipboardHistory()} />
        )}
        <span className="ml-auto flex min-w-0 items-center gap-1.5 truncate pr-1.5">
          <span aria-hidden className="h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-500" />
          {t("clipboard.footer.kept", { device: deviceName })}
        </span>
      </footer>
      {error && <p className="clip-foot px-4 pb-2 text-[12px] text-status-error">{error}</p>}
    </main>
  );
}

function FooterAction({
  keys,
  label,
  disabled,
  onClick,
}: {
  keys: string;
  label: string;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="inline-flex items-center gap-1 rounded px-1 py-0.5 hover:bg-row hover:text-ink-secondary disabled:pointer-events-none disabled:opacity-50"
    >
      <span className="clip-key rounded px-1 font-mono text-[11px]">{keys}</span>
      {label}
    </button>
  );
}
