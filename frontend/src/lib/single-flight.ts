// One write at a time, newest state wins.
//
// A setting that is written from several places (the dock after a drag, the
// library on a click, the title bar on a switch) can be asked for again while a
// write is still in flight, and two writes racing could leave the older one on
// disk. This runs `run` one at a time; requests that arrive meanwhile collapse
// into one follow-up, and since `run` reads the state when it *starts*, that
// follow-up carries the newest of them.

export interface SingleFlight {
	/** Ask for a write: now if none is running, otherwise right after it. */
	request(): void;
	/** Resolves once nothing is running and nothing is waiting. */
	idle(): Promise<void>;
}

export function singleFlight(run: () => Promise<unknown>): SingleFlight {
	let running = false;
	let again = false;
	let done: Promise<void> = Promise.resolve();

	async function loop() {
		running = true;
		try {
			do {
				again = false;
				try {
					await run();
				} catch {
					// A failed write is the writer's to report; the queue must go on.
				}
			} while (again);
		} finally {
			running = false;
		}
	}

	return {
		request() {
			if (running) {
				again = true;
				return;
			}
			done = loop();
		},
		idle: () => done
	};
}
