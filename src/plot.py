"""Draw one chart from sampled points. Member text is data, never code."""

import json
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

COLORS = ("#3d7ea6", "#c46b3a", "#5c8a4a")
MARKERS = ("o", "s", "^")


def main():
    spec = json.load(sys.stdin)
    kind = spec.get("kind") or "line"
    series = spec.get("series") or []
    figure, axis = plt.subplots(figsize=(7.2, 4.4), dpi=120)
    if kind == "bar":
        draw_bars(axis, series[0])
    else:
        draw_points(axis, series, connect=kind != "scatter")
    apply_labels(axis, spec, series, kind)
    axis.grid(True, linestyle=":", linewidth=0.6, alpha=0.7)
    figure.tight_layout()
    figure.savefig(sys.stdout.buffer, format="png", facecolor="white")
    plt.close(figure)


def draw_bars(axis, series):
    points = series.get("points") or []
    labels = [point_label(point) for point in points]
    values = [point.get("y", 0) for point in points]
    axis.bar(range(len(values)), values, color=COLORS[0])
    axis.set_xticks(range(len(labels)))
    rotate = any(len(label) > 6 for label in labels)
    axis.set_xticklabels(labels, rotation=30 if rotate else 0, ha="right" if rotate else "center")
    axis.axhline(0, color="#444444", linewidth=0.8)


def draw_points(axis, series, connect):
    named = False
    for index, item in enumerate(series):
        points = item.get("points") or []
        xs = [point.get("x", 0) for point in points]
        ys = [point.get("y", 0) for point in points]
        name = item.get("name") or None
        named = named or bool(name)
        color = COLORS[index % len(COLORS)]
        marker = MARKERS[index % len(MARKERS)]
        if connect:
            axis.plot(xs, ys, color=color, marker=marker, label=name)
        else:
            axis.scatter(xs, ys, color=color, marker=marker, label=name)
    if len(series) == 1:
        points = series[0].get("points") or []
        labels = [point.get("label") for point in points]
        if points and all(isinstance(label, str) and label for label in labels):
            axis.set_xticks([point.get("x", 0) for point in points])
            axis.set_xticklabels(labels, rotation=30 if any(len(label) > 6 for label in labels) else 0)
    if named:
        axis.legend()


def apply_labels(axis, spec, series, kind):
    title = text_field(spec, "title")
    expression = text_field(spec, "expression")
    if title is None and kind == "bar" and series:
        title = text_field(series[0], "name")
    if expression and title:
        axis.set_title(title + "\n" + "y = " + expression)
    elif expression:
        axis.set_title("y = " + expression)
    elif title:
        axis.set_title(title)
    x_label = text_field(spec, "x_label")
    y_label = text_field(spec, "y_label")
    if x_label:
        axis.set_xlabel(x_label)
    if y_label:
        axis.set_ylabel(y_label)


def point_label(point):
    label = point.get("label")
    if isinstance(label, str) and label:
        return label
    return format_number(point.get("x", 0))


def format_number(value):
    if isinstance(value, float) and value.is_integer():
        return str(int(value))
    return str(value)


def text_field(source, key):
    value = source.get(key)
    if isinstance(value, str) and value:
        return value
    return None


if __name__ == "__main__":
    main()
