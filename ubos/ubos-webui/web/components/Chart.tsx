import { useEffect, useRef, useState } from 'preact/hooks';
import type { TargetedEvent } from 'preact';
import { SAMPLES } from '../lib/store';
import { niceStep } from '../lib/format';

// Axis gutters, in coordinate-space pixels (1 unit = 1 CSS px because
// the viewBox is set from the measured size).
const PAD_LEFT = 38;
const PAD_RIGHT = 12;
const PAD_TOP = 10;
const PAD_BOTTOM = 20;

export interface ChartLine {
  buf: (number | null)[];
  color: string;
  label: string;
  /** The line's name in the tooltip, where it says more than `label`. */
  tip?: string;
}

export interface ChartProps {
  lines: ChartLine[];
  min: number;
  max: number;
  minSpan: number;
  axis: (v: number) => string;
  val: (v: number) => string;
  cursor: number | null;
  onCursor: (index: number) => void;
}

interface GridLine {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

interface AxisLabel {
  x: number;
  y: number;
  text: string;
}

interface PathSeg {
  d: string;
  color: string;
}

interface CursorDot {
  cx: string;
  cy: string;
  color: string;
}

interface CursorTip {
  x1: string;
  y1: number;
  x2: string;
  y2: number;
  dots: CursorDot[];
}

interface Geo {
  viewBox: string;
  gridLines: GridLine[];
  yLabels: AxisLabel[];
  xLabels: AxisLabel[];
  paths: PathSeg[];
  tip: CursorTip | null;
}

interface TooltipEntry {
  label: string;
  color: string;
  text: string;
}

interface Tooltip {
  age: string;
  entries: TooltipEntry[];
  x: number;
  y: number;
  flip: boolean;
}

export default function Chart({
  lines,
  min,
  max: maxProp,
  minSpan,
  axis,
  val,
  cursor,
  onCursor,
}: ChartProps) {
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [w, setW] = useState(0);
  const [h, setH] = useState(0);

  useEffect(() => {
    const svgEl = svgRef.current;
    if (!svgEl) return;
    const measure = () => {
      setW(Math.round(svgEl.clientWidth));
      setH(Math.round(svgEl.clientHeight));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(svgEl);
    return () => ro.disconnect();
  }, []);

  function handlePointerMove(event: TargetedEvent<SVGSVGElement, PointerEvent>) {
    if (event.pointerType !== 'mouse') return;
    if (!w) return;
    const svgEl = svgRef.current;
    if (!svgEl) return;
    const plotLeft = PAD_LEFT;
    const plotWidth = w - PAD_LEFT - PAD_RIGHT;
    const rect = svgEl.getBoundingClientRect();
    // clientWidth equals the viewBox width (1 unit = 1 CSS pixel), so no
    // scale factor is needed beyond the element's own bounding rect.
    const viewX = (event.clientX - rect.left) * (w / rect.width);
    let index = Math.round(((viewX - plotLeft) * (SAMPLES - 1)) / plotWidth);
    if (index < 0) index = 0;
    if (index >= SAMPLES) index = SAMPLES - 1;
    onCursor(index);
  }

  let geo: Geo | null = null;
  if (w && h) {
    const plotLeft = PAD_LEFT;
    const plotTop = PAD_TOP;
    const plotWidth = w - PAD_LEFT - PAD_RIGHT;
    const plotHeight = h - PAD_TOP - PAD_BOTTOM;

    // Auto-range the Y top across every line: rescale when the peak
    // leaves the 30–95% band so the trace sits near mid-axis; min fixed.
    let peak: number | null = null;
    for (const line of lines) {
      for (const value of line.buf) {
        if (value == null) continue;
        if (peak == null || value > peak) peak = value;
      }
    }
    let max = maxProp;
    if (peak != null) {
      const frac = (peak - min) / (max - min);
      if (frac < 0.3 || frac > 0.95) {
        const desiredSpan = Math.max((peak - min) * 2, minSpan);
        const majStep = niceStep(desiredSpan / 4);
        const divs = Math.max(2, Math.ceil(desiredSpan / majStep));
        max = min + majStep * divs;
      }
    }

    const span = max - min;
    const yBottom = plotTop + plotHeight;

    const gridLines: GridLine[] = [];
    gridLines.push({ x1: plotLeft, y1: yBottom, x2: plotLeft + plotWidth, y2: yBottom });
    gridLines.push({ x1: plotLeft, y1: plotTop, x2: plotLeft + plotWidth, y2: plotTop });

    const yLabels: AxisLabel[] = [];
    yLabels.push({ x: 0, y: yBottom + 3, text: axis(min) });
    yLabels.push({ x: 0, y: plotTop + 3, text: axis(max) });

    const xLabels: AxisLabel[] = [];
    for (let minute = 0; minute <= 5; minute++) {
      const x = plotLeft + (minute / 5) * plotWidth;
      xLabels.push({ x, y: h - 6, text: minute === 5 ? 'Now' : `-${5 - minute}m` });
    }

    const paths: PathSeg[] = lines.map((line) => {
      let d = '';
      let penDown = false;
      for (let i = 0; i < SAMPLES; i++) {
        const value = line.buf[i];
        if (value == null) {
          penDown = false;
          continue;
        }
        const clamped = Math.min(Math.max(value, min), max);
        const x = (plotLeft + (i * plotWidth) / (SAMPLES - 1)).toFixed(1);
        const y = (plotTop + plotHeight - ((clamped - min) / span) * plotHeight).toFixed(1);
        d += (penDown ? 'L' : 'M') + x + ',' + y + ' ';
        penDown = true;
      }
      return { d, color: line.color };
    });

    // Cursor crosshair: shared hover state resolved with this render's own
    // min/max/span, since auto-ranging above may have widened max from the
    // prop and the dots must line up with the drawn paths, not the props.
    let tip: CursorTip | null = null;
    if (cursor != null) {
      const pixelX = plotLeft + (cursor * plotWidth) / (SAMPLES - 1);
      const dots = lines
        .map((line): CursorDot | null => {
          const value = line.buf[cursor];
          if (value == null) return null;
          const clamped = Math.min(Math.max(value, min), max);
          const y = (plotTop + plotHeight - ((clamped - min) / span) * plotHeight).toFixed(1);
          return { cx: pixelX.toFixed(1), cy: y, color: line.color };
        })
        .filter((dot): dot is CursorDot => dot != null);
      tip = { x1: pixelX.toFixed(1), y1: plotTop, x2: pixelX.toFixed(1), y2: plotTop + plotHeight, dots };
    }

    geo = { viewBox: `0 0 ${w} ${h}`, gridLines, yLabels, xLabels, paths, tip };
  }

  let tooltip: Tooltip | null = null;
  if (cursor != null && geo?.tip?.dots.length) {
    const secondsAgo = (SAMPLES - 1 - cursor) * 2;
    const now = new Date(Date.now() - secondsAgo * 1000);
    const age = now.toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
    const entries = lines
      .map((line): TooltipEntry | null => {
        const v = line.buf[cursor];
        if (v == null) return null;
        return { label: line.tip ?? line.label, color: line.color, text: val(v) };
      })
      .filter((e): e is TooltipEntry => e != null);
    const tipX = parseFloat(geo.tip.x1);
    const rel = (tipX - PAD_LEFT) / (w - PAD_LEFT - PAD_RIGHT);
    const minDotY = Math.min(...geo.tip.dots.map((d) => parseFloat(d.cy)));
    tooltip = { age, entries, x: tipX, y: minDotY, flip: rel > 0.75 };
  }

  return (
    <div class="chart">
      <span class="lb">
        {lines.map((line, i) => (
          <span key={i} style={{ color: line.color }}>{line.label}</span>
        ))}
      </span>
      <div class="plot">
      <svg ref={svgRef} viewBox={geo?.viewBox} onPointerMove={handlePointerMove}>
        <g class="g">
          {geo &&
            geo.gridLines.map((line, i) => (
              <line key={i} class="gr" x1={line.x1} y1={line.y1} x2={line.x2} y2={line.y2} />
            ))}
        </g>
        <g class="y">
          {geo &&
            geo.yLabels.map((t, i) => (
              <text key={i} class="ax" x={t.x} y={t.y} text-anchor="start">
                {t.text}
              </text>
            ))}
        </g>
        <g class="x">
          {geo &&
            geo.xLabels.map((t, i) => (
              <text key={i} class="ax" x={t.x} y={t.y} text-anchor="middle">
                {t.text}
              </text>
            ))}
        </g>
        <g class="lines">
          {geo &&
            geo.paths.map((p, i) => (
              <path
                key={i}
                style={{ fill: 'none', stroke: p.color, strokeWidth: 1.5, vectorEffect: 'non-scaling-stroke' }}
                d={p.d}
              />
            ))}
        </g>
        <g class="tip">
          {geo?.tip && (
            <>
              <line class="cur" x1={geo.tip.x1} y1={geo.tip.y1} x2={geo.tip.x2} y2={geo.tip.y2} />
              {geo.tip.dots.map((dot, i) => (
                <circle key={i} r="3" cx={dot.cx} cy={dot.cy} style={{ fill: dot.color }} />
              ))}
            </>
          )}
        </g>
      </svg>
        {tooltip && (
          <div
            class="tt"
            style={{
              left: (tooltip.flip ? tooltip.x - 10 : tooltip.x + 10) + 'px',
              top: tooltip.y + 'px',
              transform: tooltip.flip ? 'translate(-100%, -50%)' : 'translateY(-50%)',
            }}
          >
            <div class="tt-time">{tooltip.age}</div>
            {tooltip.entries.map((e, i) => (
              <div class="tt-row" key={i}>
                <span class="tt-dot" style={{ background: e.color }}></span>
                <span class="tt-label">{e.label}</span>
                <span class="tt-val">{e.text}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
