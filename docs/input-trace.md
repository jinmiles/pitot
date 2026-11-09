# Input trace widget

Shows throttle (green), brake (red, orange while ABS is active), clutch (blue, drawn as `1 - Clutch` so a
pressed pedal is high), and steering (grey, half a turn either way) for the last few seconds, with live bars on
the right. The bars follow the pedals from left to right: clutch, brake, throttle, then the steering marker.

## How it draws

- The trace is a scrolling layer: a ring surface where each sample owns two columns. A frame draws only the
  columns of samples that arrived since the last frame, and scrolling moves the two composition visuals that
  show the ring. Nothing else is redrawn.
- Samples come from the pipeline's sample ring, so every 60 Hz sample is drawn even though the screen updates
  less often: a frame draws all samples queued since the previous one.
- The bars are redrawn only when a level changes by at least a pixel.
- Samples lost upstream (overwritten in the sim's buffers) are counted; a red line on top of the bars appears if
  any were lost.
- Hidden unless the player is driving.

## Update rate

`rate_hz` sets how often the screen updates (default 60, for a smooth trace). Development measurement on the lab PC (release build,
60 Hz replay, process CPU time over 10 s, not `pitot-bench`):

| Screen updates | CPU, one core | CPU, all 8 cores |
|---|---|---|
| 30 Hz | 0.78% | 0.098% |
| 60 Hz | 2.34% | 0.293% |
| widget disabled (status only) | 0.16% | 0.020% |

The trace is the most expensive widget at 60 Hz. Setting `rate_hz = 30` halves the screen updates and costs about
a third of the CPU, still drawing every sample. The defaults favor a smooth input trace and update the relative and
standings less often (10 Hz and 2 Hz).
