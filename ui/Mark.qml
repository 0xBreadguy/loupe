import QtQuick
import qs.Commons

// The bar mark: an optional count, then the dot cell or the diamond, laid out
// the way Omarchy's BarIconButton lays out its labelText and icon (the label
// first, Style.space(4), then a Style.bar.iconCanvas square centered in the
// slot), so it sits on the line the stock icons share.
//   kind: "cell" (idle, starting, running) or "need" (an agent needs you).
// The broken state's alert icon is drawn by the button itself, fitted the way
// every stock icon is, so it isn't here.
Item {
  id: root
  property var button: null
  property string kind: "cell"
  property int lit: 0
  property string count: ""
  property bool dim: false

  readonly property color ink: !button ? "white" : kind === "need" ? button.activeColor : button.foreground
  readonly property real canvas: Style.bar.iconCanvas
  // The gap is measured to the mark's ink, not to its 16 px canvas: a 5 px wide
  // cell centered in that canvas would leave the count floating away from it.
  // The diamond is an 8 px square turned 45 degrees, about 11 px across.
  readonly property real inkWidth: kind === "need" ? 11 : cell.width
  readonly property real labelWidth: count === "" ? 0 : label.implicitWidth + Style.space(4)
  // What the widget needs: the stock slot, plus the label when there is one.
  readonly property real slotWidth: Style.bar.iconSlot + labelWidth

  anchors.fill: parent
  // Omarchy's 0.45 for a quiet widget (WidgetButton.qml `dimmed`).
  opacity: dim ? 0.45 : 1
  Behavior on opacity { NumberAnimation { duration: Style.duration(140); easing.type: Easing.OutCubic } }

  Item {
    id: group
    width: root.labelWidth + (root.count === "" ? root.canvas : root.inkWidth)
    height: root.canvas
    x: Math.round((root.width - width) / 2)
    y: Math.round((root.height - height) / 2)
    scale: root.button ? root.button.pressScale : 1

    Text {
      id: label
      visible: root.count !== ""
      textFormat: Text.PlainText
      text: root.count
      color: root.ink
      font.family: root.button ? root.button.fontFamily : Style.font.family
      font.pixelSize: root.button ? root.button.fontSize : Style.bar.iconFont
      renderType: Text.NativeRendering
      // BarIconButton's label rule: center the figure height, not the line box,
      // which keeps room for descenders and would put the digits high.
      y: (parent.height - height) / 2
        + (metrics.descent - metrics.ascent + metrics.tightBoundingRect("0").height) / 2
    }
    FontMetrics { id: metrics; font: label.font }

    Item {
      x: root.labelWidth - (root.count === "" ? 0 : (root.canvas - root.inkWidth) / 2)
      width: root.canvas
      height: root.canvas

      DotCell {
        id: cell
        visible: root.kind === "cell"
        x: Math.round((parent.width - width) / 2)
        y: Math.round((parent.height - height) / 2)
        lit: root.lit
        // At rest the whole cell is drawn and the mark dimmed; ghost dots under
        // that dim would all but vanish on a light bar.
        ghost: root.dim ? 1 : 0.28
        color: root.ink
      }

      Rectangle {
        visible: root.kind === "need"
        width: 8
        height: 8
        x: Math.round((parent.width - width) / 2)
        y: Math.round((parent.height - height) / 2)
        rotation: 45
        antialiasing: true
        color: root.ink
      }
    }
  }
}
