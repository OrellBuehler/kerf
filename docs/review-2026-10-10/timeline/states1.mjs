import { launch, open, shot } from './lib.mjs';
const b = await launch();
const { p } = await open(b, { q: '?music=1', dsf: 1 });
const TL = { x: 0, y: 665, width: 1920, height: 390 };
const clip = async (id) => (await p.locator(`.kclip[data-clip-id="${id}"]`).boundingBox());
// click ruler to seek to ~3s, set in; seek to ~9s set out; add marker at 6s
const ruler = { x: 224, y: 782 };
await p.mouse.click(ruler.x + 36 * 3, ruler.y);
await p.keyboard.press('i');
await p.mouse.click(ruler.x + 36 * 6, ruler.y);
await p.keyboard.press('m');
await p.mouse.click(ruler.x + 36 * 9, ruler.y);
await p.keyboard.press('o');
await p.waitForTimeout(400);
// add 2 titles via + button
await p.mouse.click(ruler.x + 36 * 14, ruler.y);
await p.locator('button[aria-label="Add a title"]').click();
await p.waitForTimeout(300);
await p.mouse.click(ruler.x + 36 * 2, ruler.y);
await p.locator('button[aria-label="Add a title"]').click();
await p.waitForTimeout(500);
await shot(p, 'st-marks-titles', TL);
await shot(p, 'st-marks-titles-full');
// header states: lock V1, mute A1, solo A2
await p.locator('button[aria-label="Toggle lock"]').nth(0).click();
await p.locator('button[aria-label="Toggle mute"]').nth(1).click();
await p.locator('button[aria-label="Toggle solo"]').nth(2).click();
await p.locator('button[aria-label="Toggle ducking"]').nth(1).click();
await p.waitForTimeout(400);
await shot(p, 'st-headers', TL);
await shot(p, 'st-headers-crop', { x: 0, y: 790, width: 520, height: 270 });
console.log(p.logs);
await b.close();
