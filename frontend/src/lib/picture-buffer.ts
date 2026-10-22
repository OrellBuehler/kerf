/* The Preview's streamed and settled pictures through two stacked `<img>`s, so a new one is never
 * painted before it has been decoded.
 *
 * One `<img>` whose `src` is replaced shows nothing but its own background for the frames it takes to
 * decode the new data URL, and the matte behind the picture is black in every theme: a black flash, and an
 * animated title that FFmpeg burns into the streamed JPEGs flickered with it. Here the next picture goes
 * into the slot that is not on show, `decode()` resolves when it can be painted, and only then does it
 * become the one on show. This is the decision and nothing else; the element, and what "loaded" means for
 * it, are the caller's (`load`), so it can be tested without a webview.
 *
 *  - One decode at a time; the newest picture asked for waits. A picture is shown in the order it was
 *    asked for, so a streamed frame can never land over a newer settled still.
 *  - A decode that finishes while a newer picture is waiting is dropped, not shown, up to `MAX_DROPS` in a
 *    row: a machine that decodes slower than the stream's rate would otherwise never show anything.
 *  - A decode that fails changes nothing: the picture on show stays.
 *  - A picture that replaces the GPU's (`over`) is not shown over what the page had before the GPU took
 *    the pane, which is seconds old, so the old one is hidden (`covered`) until the new one is ready. */

export type Slot = 0 | 1;

/** Decodes dropped in a row, for a newer picture waiting, before the one that finished is shown anyway. */
export const MAX_DROPS = 2;

export interface PictureBufferDeps {
	/** Put `url` in the slot that is not on show; resolves once it can be painted without delay, rejects
	 *  when it cannot be decoded. */
	load(slot: Slot, url: string): Promise<void>;
	/** What is on show changed: the slot that holds the picture, `null` for none, and whether the one
	 *  that was there is being kept hidden until its replacement is ready. */
	show(slot: Slot | null, covered: boolean): void;
}

export function createPictureBuffer(deps: PictureBufferDeps) {
	/** The newest decoded picture; its slot is the one on show unless it is covered. */
	let last: { slot: Slot; url: string } | null = null;
	let spare: Slot = 0;
	let covered = false;
	let want: string | null = null;
	let loading = false;
	let drops = 0;
	let epoch = 0;

	function emit() {
		deps.show(covered ? null : (last?.slot ?? null), covered);
	}

	function done(mine: number, slot: Slot, url: string, ok: boolean) {
		if (mine !== epoch) return;
		loading = false;
		if (!ok) {
			drops = 0;
			// The picture that was on show stays; nothing is to be waited for any more.
			if (!want && covered) {
				covered = false;
				emit();
			}
		} else if (want && drops < MAX_DROPS) drops++;
		else {
			drops = 0;
			last = { slot, url };
			spare = slot === 0 ? 1 : 0;
			covered = false;
			emit();
		}
		pump();
	}

	function pump() {
		if (loading || want === null) return;
		const url = want;
		want = null;
		// Already on show (a paused video repeats its frame).
		if (!covered && last?.url === url) return;
		loading = true;
		const mine = epoch;
		const slot = spare;
		deps.load(slot, url).then(
			() => done(mine, slot, url, true),
			() => done(mine, slot, url, false)
		);
	}

	return {
		/** The next picture. `over`: it replaces what the GPU's surface showed. */
		push(url: string, over = false) {
			if (over && !covered) {
				covered = true;
				emit();
			}
			want = url;
			pump();
		},
		/** Nothing is to be shown (the elements are gone or blank): forget the pictures, drop the decode in flight. */
		reset() {
			epoch++;
			last = null;
			spare = 0;
			covered = false;
			want = null;
			loading = false;
			drops = 0;
			emit();
		}
	};
}
