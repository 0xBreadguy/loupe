import QtQuick

// The dashboard's braille spinner cell, drawn as geometry: two columns of four
// dots on whole pixels. Unlit dots stay drawn faintly, so the cell's footprint
// and weight are the same in every frame, at rest and while running. A font's
// braille glyph can't do that: each frame lights different dots, so its ink
// jumps around inside the cell, and it sits a few pixels above the bar's icons.
// `lit` is a braille bitmask: bit 0 is dot 1 ... bit 7 is dot 8, Unicode's order.
Item {
  id: root
  property int lit: 0
  property color color: "white"
  property real ghost: 0.28
  property int dot: 2
  property int gap: 1

  implicitWidth: dot * 2 + gap
  implicitHeight: dot * 4 + gap * 3
  width: implicitWidth
  height: implicitHeight

  // Dots 1-3 run down the left column, 4-6 down the right, 7 and 8 sit underneath.
  readonly property var cols: [0, 0, 0, 1, 1, 1, 0, 1]
  readonly property var rows: [0, 1, 2, 0, 1, 2, 3, 3]

  Repeater {
    model: 8
    Rectangle {
      required property int index
      x: root.cols[index] * (root.dot + root.gap)
      y: root.rows[index] * (root.dot + root.gap)
      width: root.dot
      height: root.dot
      // Small dots stay square and unsmoothed so they land on whole pixels.
      radius: root.dot >= 4 ? root.dot / 2 : 0
      antialiasing: root.dot >= 4
      color: root.color
      opacity: ((root.lit >> index) & 1) ? 1 : root.ghost
    }
  }
}
