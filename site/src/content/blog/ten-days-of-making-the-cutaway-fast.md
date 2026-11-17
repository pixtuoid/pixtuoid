---
title: Ten days of making the cutaway fast
description: The cutaway draws the office as real pixels inside a terminal. Its first measured night frame took 43 ms of a 33 ms budget. This is what brought it under, what did not, and the gate that holds it there.
pubDate: 2026-11-17
---

pixtuoid has two looks. The classic one paints the office with half-block
characters, two pixels to a terminal cell. The cutaway paints it as an image:
a detailed pixel view of the office, lit by its own lamps at night, with
weather outside its windows, sent to the terminal through the kitty, SIXEL or
iTerm2 image protocols.

An image costs far more than text. The office redraws about thirty times a
second, so a frame has 33 ms to be painted, encoded, written and shown. The
work of fitting inside those 33 ms ran from September 30 to October 9. On
October 21 the cutaway became the default look ([#1835](https://github.com/pixtuoid/pixtuoid/pull/1835)).

Every number below is quoted from the pull request it links to. They were
measured on different days, machines and terminals, so each pair compares only
with itself: do not add them up.

## Measure before changing anything

The first pull request of the ten days changed no behaviour. It added a bench
that paints a busy office at noon and at night
([#1100](https://github.com/pixtuoid/pixtuoid/pull/1100)). Night was the slow
one: every lamp and screen lights the pixels around it.

A profile of the night frame then showed three places the time went: a
per-pixel colour cache behind a slow hash, copies made around every piece of
furniture to see what it had lit, and rebuilding the list of things to draw.
[#1134](https://github.com/pixtuoid/pixtuoid/pull/1134) rewrote all three.

<!-- figure: bars -->

| Measure                                                                          | Before  | After  |
| -------------------------------------------------------------------------------- | ------- | ------ |
| Painting a night frame ([#1134](https://github.com/pixtuoid/pixtuoid/pull/1134)) | 43.4 ms | 9.9 ms |
| Painting a noon frame ([#1134](https://github.com/pixtuoid/pixtuoid/pull/1134))  | 13.0 ms | 8.1 ms |

## Encode on every core

Painting was one cost. Turning the painted frame into bytes a terminal accepts
was another, and it ran on one core.
[#1282](https://github.com/pixtuoid/pixtuoid/pull/1282) cut the frame into
tiles and encoded them across the machine's cores. The same pull request added
the pacing bench that found the problem: it runs the office through a storm
and reports how long frames really took.

<!-- figure: bars -->

| Measure                                                                                | Before | After  |
| -------------------------------------------------------------------------------------- | ------ | ------ |
| Encoding one frame for kitty ([#1282](https://github.com/pixtuoid/pixtuoid/pull/1282)) | 34 ms  | 9.5 ms |

## Paint only what changed

Most frames barely differ from the one before: a cursor blinks, a fish swims.
The painter already knew which rectangles had changed, but it used them only
to decide what to send. It still repainted everything, about 6.8 million
pixels for a median of 7,936 that could change
([#1389](https://github.com/pixtuoid/pixtuoid/pull/1389)).

Three changes fixed that. A frame first repainted only the box around its
changes, then up to eight boxes
([#1402](https://github.com/pixtuoid/pixtuoid/pull/1402)), and it began to
reuse the last frame's windows and lights when it would draw the same ones
([#1395](https://github.com/pixtuoid/pixtuoid/pull/1395)).

<!-- figure: bars -->

| Measure                                                                                            | Before  | After   |
| -------------------------------------------------------------------------------------------------- | ------- | ------- |
| A typical noon frame, one box ([#1389](https://github.com/pixtuoid/pixtuoid/pull/1389))            | 8.26 ms | 3.80 ms |
| A typical night frame, eight boxes ([#1402](https://github.com/pixtuoid/pixtuoid/pull/1402))       | 4.78 ms | 1.70 ms |
| Composing a noon frame, reusing the last ([#1395](https://github.com/pixtuoid/pixtuoid/pull/1395)) | 1.77 ms | 0.48 ms |

Reuse is not free. Keeping the last frame's windows and lights raised the
peak memory of a 360x240 render from 562.6 KB to 639.5 KB, and the pull
request says so.

## Send fewer bytes, and fewer images

An idle office was still streaming about 1.3 MB a second to the terminal,
because every tile went out uncompressed. Compressing them made a typical tile
about ten times smaller
([#1197](https://github.com/pixtuoid/pixtuoid/pull/1197)). SIXEL and iTerm2
got faster encoders of their own
([#1400](https://github.com/pixtuoid/pixtuoid/pull/1400)).

<!-- figure: bars -->

| Measure                                                                                  | Before  | After   |
| ---------------------------------------------------------------------------------------- | ------- | ------- |
| Encoding a whole frame, SIXEL ([#1400](https://github.com/pixtuoid/pixtuoid/pull/1400))  | 35.8 ms | 17.9 ms |
| Encoding a whole frame, iTerm2 ([#1400](https://github.com/pixtuoid/pixtuoid/pull/1400)) | 24.3 ms | 11.1 ms |

Then came the change that taught us the most. For a terminal on the same
machine, [#1423](https://github.com/pixtuoid/pixtuoid/pull/1423) passed tiles
through shared memory, and the bytes written per frame fell from about 275 KB
to about 15 KB. The frame time barely moved: the 99th-percentile frame went
from 157 ms to 140 ms.
In a maximized Ghostty window the cost was not the bytes. It was the number of
images on screen.

So [#1445](https://github.com/pixtuoid/pixtuoid/pull/1445) made tiles grow
until a whole frame fits a fixed budget of images. A small window keeps small
tiles and sends only what changed; a big one uses bigger tiles.

<!-- figure: bars -->

| Measure                                                                                                       | Before   | After   |
| ------------------------------------------------------------------------------------------------------------- | -------- | ------- |
| 99th-percentile frame in a storm, maximized Ghostty ([#1445](https://github.com/pixtuoid/pixtuoid/pull/1445)) | 148.9 ms | 35.5 ms |
| 99th-percentile frame at dusk, maximized Ghostty ([#1445](https://github.com/pixtuoid/pixtuoid/pull/1445))    | 74.8 ms  | 6.1 ms  |

## The window, too

`pixtuoid floating` shows the same office in a window of its own. On a 4K
display it copied the whole window to the screen for every frame.
[#1524](https://github.com/pixtuoid/pixtuoid/pull/1524) moved it onto the GPU.
Its numbers are in milliseconds of lateness for every second the office runs,
the measure the last section explains.

<!-- figure: bars -->

| Measure                                                                                           | Before     | After    |
| ------------------------------------------------------------------------------------------------- | ---------- | -------- |
| Frames shown late in a storm, 4K window ([#1524](https://github.com/pixtuoid/pixtuoid/pull/1524)) | 235.9 ms/s | 1.0 ms/s |
| Frames shown late at dusk, 4K window ([#1524](https://github.com/pixtuoid/pixtuoid/pull/1524))    | 167.4 ms/s | 0.1 ms/s |

That one cost something as well: the release binary grew from 7,948,640 to
11,266,240 bytes, and a clean build went from about 80 seconds to 105.

## What did not work

Several ideas were tried, measured and dropped, and the pull requests record
them beside what stayed.

- **A thread pool.** The obvious way to encode across cores is a library pool
  that keeps its threads alive. Measured against starting fresh threads for
  each frame over two runs, the pool's 99th-percentile encode took 34.7 ms
  and 48.7 ms where the fresh threads took 21.9 ms and 27.7 ms: work sat
  waiting on workers that had gone to sleep. It was reverted
  ([#1497](https://github.com/pixtuoid/pixtuoid/pull/1497)).
- **Sending part of a frame.** A budget that sends some SIXEL tiles now and
  the rest later was rejected before it was built: half a lightning strike
  sent is a partial flash
  ([#1400](https://github.com/pixtuoid/pixtuoid/pull/1400)).
- **Shared memory as the fix.** It removed the bytes and did not make frames
  on time, as above. It stayed, for the CPU it saves, but it was not the
  answer it looked like.

## The gate that holds it

A bench says how fast one frame can be. It does not say whether the office
looks smooth, because a frame that is ready early still waits for its turn,
and a frame that is late shows late.

The first gate asked for a 99th-percentile frame of 20 ms or less and no
frame past its interval
([#1385](https://github.com/pixtuoid/pixtuoid/pull/1385)). The 20 ms was one
machine's number. [#1497](https://github.com/pixtuoid/pixtuoid/pull/1497)
replaced it with the hitch rate, the measure
[Apple uses](https://developer.apple.com/documentation/xcode/understanding-hitches-in-your-app)
for the same question: how many milliseconds late frames were shown, for
every second the office ran. Ten or fewer passes.

`just pace-check` runs the release binary through a storm and across dusk and
fails above that bar. It runs on a developer's machine before a release and
never in CI: a shared runner's clock is too noisy to judge a 33 ms budget.
