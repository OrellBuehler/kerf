/* The notification log (Svelte 5 runes).
 *
 * Kerf reports everything through toasts, and a toast is gone in a few seconds
 * — which is fine for "Clip copied" and useless for "the speech model could not
 * be downloaded: <reason>", the notice you actually needed to read. So every
 * toast is *also* recorded here, and the title bar's bell opens the log.
 *
 * The wrapper below is a drop-in for `svelte-sonner`'s `toast`: components
 * import it from here instead, and nothing about the call sites changes. A
 * toast that is never recorded is a toast that can be lost, so the swap is
 * whole-app rather than per-message.
 *
 * The log is deliberately *not* replayable — no action buttons are kept. A
 * toast's "Undo" undoes whatever the newest revision is, which an hour later is
 * not the edit the notice was about. What a notice about a file can keep is the
 * file's path as data (`reveal`): the log then offers to show it in its folder,
 * which is as true an hour later as it was when the file was written. */

import { toast as sonner, type ExternalToast } from 'svelte-sonner';
import { logFrontend } from './log';

export type NoticeKind = 'success' | 'error' | 'warning' | 'info' | 'note';

export interface Notice {
	id: number;
	kind: NoticeKind;
	text: string;
	/** The reason, under the line — the part of a failure worth reading afterwards. */
	description?: string;
	/** A file the notice is about: the log offers to show it in its folder (`reveal_path`). */
	reveal?: string;
	/** Epoch ms, so the panel can age it. */
	at: number;
	read: boolean;
}

/** How much history to keep. Old enough to have scrolled away is old enough
 *  to drop; this is a safety net for what you missed, not an audit log. */
const MAX = 200;

/** Errors linger: the default four seconds is what lost the message in the
 *  first place, and a failure usually carries a path or a reason to read. */
const ERROR_MS = 12_000;
const WARNING_MS = 8_000;

class Notifications {
	/** Newest first. */
	items = $state<Notice[]>([]);
	/** Whether the panel is showing. */
	open = $state(false);

	#nextId = 1;

	get unread(): number {
		return this.items.reduce((n, i) => n + (i.read ? 0 : 1), 0);
	}

	/** Whether anything unread is a failure — the badge shouts for those and
	 *  stays quiet for a pile of "Clip pasted". */
	get unreadProblem(): boolean {
		return this.items.some((i) => !i.read && (i.kind === 'error' || i.kind === 'warning'));
	}

	record(kind: NoticeKind, text: string, description?: string, reveal?: string) {
		this.items.unshift({
			id: this.#nextId++,
			kind,
			text,
			...(description ? { description } : {}),
			...(reveal ? { reveal } : {}),
			at: Date.now(),
			read: false
		});
		if (this.items.length > MAX) this.items.length = MAX;
	}

	markRead(id: number, read = true) {
		const n = this.items.find((i) => i.id === id);
		if (n) n.read = read;
	}

	markAllRead() {
		for (const n of this.items) n.read = true;
	}

	remove(id: number) {
		this.items = this.items.filter((i) => i.id !== id);
	}

	clear() {
		this.items = [];
	}

	toggle() {
		this.open = !this.open;
	}
}

export const notifications = new Notifications();

/** A toast's options, and two the log keeps and the toast does not draw: `reveal`, the file the notice is about, and `detail`,
 *  the long part of a reason (ffmpeg's last lines) that would fill the screen as a toast but is what the log is for. */
type Opts = ExternalToast & { reveal?: string; detail?: string };

function show(kind: NoticeKind, message: string, given?: Opts) {
	const { reveal, detail, ...opts } = given ?? {};
	notifications.record(
		kind,
		message,
		typeof opts.description === 'string' ? opts.description : typeof detail === 'string' && detail ? detail : undefined,
		typeof reveal === 'string' && reveal ? reveal : undefined
	);
	if (kind === 'error' || kind === 'warning') {
		const detail = typeof opts.description === 'string' ? ` — ${opts.description}` : '';
		logFrontend(kind === 'error' ? 'error' : 'warn', `${message}${detail}`, 'toast');
	}
	const linger = kind === 'error' ? ERROR_MS : kind === 'warning' ? WARNING_MS : undefined;
	const o = linger !== undefined && opts.duration === undefined ? { ...opts, duration: linger } : opts;
	switch (kind) {
		case 'success':
			return sonner.success(message, o);
		case 'error':
			return sonner.error(message, o);
		case 'warning':
			return sonner.warning(message, o);
		case 'info':
			return sonner.info(message, o);
		default:
			return sonner(message, o);
	}
}

/** Drop-in for `svelte-sonner`'s `toast` that also writes the notification log. */
export const toast = Object.assign((message: string, opts?: Opts) => show('note', message, opts), {
	success: (message: string, opts?: Opts) => show('success', message, opts),
	error: (message: string, opts?: Opts) => show('error', message, opts),
	warning: (message: string, opts?: Opts) => show('warning', message, opts),
	info: (message: string, opts?: Opts) => show('info', message, opts),
	dismiss: (id?: string | number) => sonner.dismiss(id)
});
