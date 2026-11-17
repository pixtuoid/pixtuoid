---
title: Ten days of making the cutaway fast
description: 'Painting one night frame took 43 ms, and a frame gets 33. Ten days of pull requests later the cutaway was the default look. What we changed, what each change cost, and what we threw away.'
pubDate: 2026-11-17
---

pixtuoid draws your AI coding agents as coworkers in a pixel office, inside
your terminal. It has two looks. This is the same moment in both:

![The same office drawn twice: the cutaway on the left, in fine pixel detail with lit windows and shaded furniture, and the classic look on the right, in coarse blocks.](../../../../docs/images/looks-composite.png)

On the right is the classic look, made of half-block characters, two pixels
to a terminal cell. On the left is the cutaway: real pixels, sent through the
kitty, SIXEL or iTerm2 image protocol.

Pixels are prettier. They are also a great deal more work, and the office
redraws thirty times a second. **A frame gets 33 ms.**

On September 30 we measured the cutaway for the first time. Painting one
night frame took 43.4 ms. Not sending it. Just painting it.

Ten days later the whole pipeline around that frame had been rebuilt, and on
October 21 the cutaway became the default look
([#1835](https://github.com/pixtuoid/pixtuoid/pull/1835)). This is what we
changed, in the order we changed it, with what each change cost and what we
threw away.

Every number here is quoted from the pull request it links to. They come from
different days, machines and terminals. **Compare each pair with itself, and
never add them up.**

## Painting a night frame: 43.4 ms → 9.9 ms

**That's 4.4x faster**, from one pull request, merged the same day as the
measurement ([#1134](https://github.com/pixtuoid/pixtuoid/pull/1134)). Here
are both in slow motion. Every time a bar fills, a frame is painted.

<!-- figure: laps -->

| Runner              | Lap     |
| ------------------- | ------- |
| Night frame, before | 43.4 ms |
| Night frame, after  | 9.9 ms  |

It began with a pull request that changed nothing
([#1100](https://github.com/pixtuoid/pixtuoid/pull/1100)). It only added a
bench: paint a busy office at noon, paint it at night, time both.

Night was the slow one. At night every lamp and screen lights the pixels
around it.

A profile of that frame showed three places the time went:

- a per-pixel colour cache sitting behind a slow hash;
- a copy made around every piece of furniture, to see what it had lit;
- the list of things to draw, rebuilt every frame.

#1134 rewrote all three. Noon came along for free: 13.0 ms → 8.1 ms.

_`render_cutaway` bench, 240×144 at scale 4, both runs back to back on one
machine._

## Encoding a frame: 34 ms → 9.5 ms

Painting is half the job. The painted frame still has to become bytes a
terminal accepts, and that ran on one core.

<!-- figure: laps -->

| Runner              | Lap    |
| ------------------- | ------ |
| Encode on one core  | 34 ms  |
| Encode on ten cores | 9.5 ms |

[#1282](https://github.com/pixtuoid/pixtuoid/pull/1282) cut the frame into
tiles and handed them to every core the machine has. **3.6x faster**, same
bytes on the wire.

The same pull request added the tool that found the problem: a pacing bench
that walks the office through a storm and records how long every frame really
took. We used it for everything after this.

_One whole frame of 1,275 tiles, kitty protocol, zlib._

## Repainting 7,936 pixels instead of 6.8 million

Most frames barely differ from the last one. A cursor blinks. A fish swims.

The painter already knew which rectangles had changed. It used them to decide
what to **send**. Then it repainted the entire frame anyway.

<!-- figure: repaint -->

| Pass                     | Pixels      |
| ------------------------ | ----------- |
| Before: the whole frame  | 6.8 million |
| After: only what changed | 7,936       |

That is about 850 times less to paint on a typical frame
([#1389](https://github.com/pixtuoid/pixtuoid/pull/1389)). Three pull requests
got there:

<!-- figure: bars -->

| Measure                                                                                             | Before  | After   |
| --------------------------------------------------------------------------------------------------- | ------- | ------- |
| Repaint one box around the changes ([#1389](https://github.com/pixtuoid/pixtuoid/pull/1389))        | 8.26 ms | 3.80 ms |
| Repaint up to eight boxes ([#1402](https://github.com/pixtuoid/pixtuoid/pull/1402))                 | 4.78 ms | 1.70 ms |
| Reuse the last frame's windows and lights ([#1395](https://github.com/pixtuoid/pixtuoid/pull/1395)) | 1.77 ms | 0.48 ms |

_Median frame: #1389 at noon on kitty, #1402 at night on SIXEL, #1395 the
compose step at noon. Three benches, three baselines._

Why eight boxes, and not one per change? We measured it. More boxes mean less
to paint, until keeping track of the boxes costs more than it saves:

| Boxes | Night frame | Weather changing |
| ----- | ----------- | ---------------- |
| 1     | 4.78 ms     | 7.85 ms          |
| 2     | 3.64 ms     | 6.19 ms          |
| 4     | 2.46 ms     | 5.24 ms          |
| 8     | 1.70 ms     | 4.77 ms          |
| 12    | 1.63 ms     | 5.17 ms          |

Twelve is barely better at night and worse when the weather turns. Eight it
is.

Reuse is not free. Keeping the last frame around raised the peak memory of a
360×240 render from 562.6 KB to 639.5 KB. That's 12% more, and we took the
trade.

## Fewer bytes did not help. Fewer images did.

First the easy win. An idle office was streaming about 1.3 MB a second,
because every tile went out uncompressed. Compressing them made a typical
tile about 10x smaller
([#1197](https://github.com/pixtuoid/pixtuoid/pull/1197)).

SIXEL and iTerm2 got faster encoders of their own
([#1400](https://github.com/pixtuoid/pixtuoid/pull/1400)): a whole frame went
from 35.8 ms to 17.9 ms on SIXEL, and from 24.3 ms to 11.1 ms on iTerm2.

Then came the change that taught us the most.

For a terminal on the same machine,
[#1423](https://github.com/pixtuoid/pixtuoid/pull/1423) passed tiles through
shared memory. Bytes per frame fell from about 275 KB to about 15 KB.
**18x fewer.**

The slow frames barely moved. The 99th-percentile frame, the one in a
hundred you actually notice, went from 157 ms to 140 ms.

Almost nothing. In a maximized Ghostty window the terminal was not slow
because of the bytes. It was slow because of **how many images** were on
screen.

So [#1445](https://github.com/pixtuoid/pixtuoid/pull/1445) stopped sending so
many small tiles. A tile now grows until the whole frame fits a budget of 400
images. A small window keeps small tiles and sends only what changed. A big
window gets big tiles.

<!-- figure: bars -->

| Measure                      | Before   | After   |
| ---------------------------- | -------- | ------- |
| 99th-percentile frame, storm | 148.9 ms | 35.5 ms |
| 99th-percentile frame, dusk  | 74.8 ms  | 6.1 ms  |

_Maximized Ghostty, 239×123 cells at 16x, 60 seconds a run._

We kept the shared memory. It saves CPU on every frame. It just was not the
fix it looked like.

## The window: 235.9 ms/s late → 1.0 ms/s

`pixtuoid floating` shows the same office in a window of its own. On a 4K
display it copied the whole window to the screen for every frame.

[#1524](https://github.com/pixtuoid/pixtuoid/pull/1524) moved it onto the
GPU. In a storm, frames had been arriving 235.9 ms late for every second the
office ran. Now it is 1.0 ms. At dusk: 167.4 → 0.1.

The price: the release binary grew from 7,948,640 to 11,266,240 bytes, and a
clean build went from 80 seconds to 105.

_M1 Max, external 4K display, 200 seconds a run._

## What we threw away

Every one of these was built or designed, measured, and dropped. The pull
requests keep the record.

**A thread pool.** The obvious way to use every core is a library pool that
keeps its threads alive. We measured it against starting fresh threads for
each frame. The pool's 99th-percentile encode took 34.7 ms and 48.7 ms over
two runs. Fresh threads took 21.9 ms and 27.7 ms. In the pool's worst frames
only 4 to 6 of its 10 workers were running: the work sat waiting on the ones
that had gone to sleep. Reverted
([#1497](https://github.com/pixtuoid/pixtuoid/pull/1497)).

**Sending part of a frame.** A budget that sends some SIXEL tiles now and the
rest next frame was rejected before anyone built it. Half a lightning strike
sent is a partial flash
([#1400](https://github.com/pixtuoid/pixtuoid/pull/1400)).

**Tracking each colour's columns.** For SIXEL, tracking the columns each
colour spans made the pass slower, not faster: 67 µs → 106 µs
([#1417](https://github.com/pixtuoid/pixtuoid/pull/1417)).

## How we keep it fast

A bench tells you how fast one frame can be. It does not tell you whether the
office looks smooth. A frame that is ready early still waits its turn. A frame
that is late shows late.

Our first gate asked for a 99th-percentile frame under 20 ms
([#1385](https://github.com/pixtuoid/pixtuoid/pull/1385)). That 20 ms was one
machine's number.

[#1497](https://github.com/pixtuoid/pixtuoid/pull/1497) replaced it with the
**hitch rate**, the measure
[Apple uses](https://developer.apple.com/documentation/xcode/understanding-hitches-in-your-app)
for the same question: how many milliseconds late frames were shown, for
every second the office ran. Ten or fewer passes.

`just pace-check` runs the release binary into a storm and across dusk, and
fails above that bar. It runs on a developer's machine before a release, and
never in CI. A shared runner's clock is too noisy to judge a 33 ms budget.

## Try it

The cutaway is on by default wherever your terminal can draw it. Everywhere
else you get the classic look, with nothing to configure. [Install pixtuoid](/) and
open a terminal next to your agents.
