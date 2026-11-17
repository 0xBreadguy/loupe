import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../Status.js" as Status

// The card's body, in Omarchy's own panel parts (PanelHero, PanelSeparator,
// PanelSectionHeader, PanelActionButton): the state mark, title and summary with
// a button to the dashboard, then the agents that need you, then the ones
// running, then a footer for the rest. Idle and broken get a sentence each.
Column {
  id: root
  property var status: null
  property var strings: null
  property string reason: ""
  property int frame: 0
  property bool animate: true
  property var hues: null
  property string fontFamily: Style.font.family
  signal openDashboard()

  readonly property color fg: Commons.Color.popups.text
  readonly property color hot: Commons.Color.urgent
  readonly property color dim: Qt.darker(fg, 1.4)
  readonly property bool broken: !!status && !!status.error
  readonly property bool available: !!status && !status.error
  readonly property int running: available ? status.running : 0
  readonly property int waiting: available ? status.waiting : 0
  readonly property int failed: available ? status.failed : 0
  readonly property var agents: available ? status.agents : []
  readonly property var needs: agents.filter(function(a) { return a.state === "waiting" })
  readonly property var others: agents.filter(function(a) { return a.state !== "waiting" })
  // The status line lists at most eight agents; the dashboard has the rest.
  readonly property int more: Math.max(0, running + waiting - agents.length)

  function n(v) { return Commons.I18n.formatNumber(v) }
  function t(key) { return root.strings ? root.strings.t(key) : "" }
  function tn(key, count) { return root.strings ? root.strings.tn(key, count).arg(root.n(count)) : "" }

  readonly property string summary: broken ? t("card.broken")
    : !available ? t("card.starting")
    : (running + waiting === 0) ? t("card.idle")
    : [waiting > 0 ? tn("summary.waiting", waiting) : "", running > 0 ? tn("summary.running", running) : ""]
      .filter(function(s) { return s }).join(" · ")

  spacing: Style.space(10)

  PanelHero {
    width: root.width
    title: root.t("card.title")
    meta: root.summary
    foreground: root.fg
    fontFamily: root.fontFamily
    iconComponent: Component {
      Item {
        implicitWidth: Style.font.display
        implicitHeight: Style.font.display
        DotCell {
          visible: !root.broken && root.waiting === 0
          anchors.centerIn: parent
          dot: 4
          gap: 2
          lit: Status.litDots(root.status, root.frame, root.animate)
          color: root.fg
        }
        Rectangle {
          visible: !root.broken && root.waiting > 0
          anchors.centerIn: parent
          width: 15; height: 15; rotation: 45; antialiasing: true
          color: root.hot
        }
        // The Nerd Font alert icon, as the bar's broken mark.
        OpticalGlyph {
          visible: root.broken
          anchors.fill: parent
          text: "\u{f0026}"
          fontFamily: root.fontFamily
          fontSize: Style.font.display
          paintedSize: Style.font.display
          color: root.fg
        }
      }
    }
    trailingControl: Component {
      PanelActionButton {
        iconText: "\u{f07b7}"
        tooltipText: root.t("card.open")
        bordered: true
        foreground: root.fg
        fontFamily: root.fontFamily
        onClicked: root.openDashboard()
      }
    }
  }

  PanelSeparator { foreground: root.fg }

  // Broken: what went wrong, and that it keeps trying.
  Text {
    visible: root.broken
    width: root.width
    wrapMode: Text.Wrap
    textFormat: Text.PlainText
    text: root.reason
    color: root.fg
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
  }
  Text {
    visible: root.broken
    width: root.width
    wrapMode: Text.Wrap
    textFormat: Text.PlainText
    text: root.t("card.retry")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }

  // Starting: the watcher hasn't reported yet.
  Text {
    visible: !root.status
    width: root.width
    wrapMode: Text.Wrap
    textFormat: Text.PlainText
    text: root.t("card.waitingForWatcher")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
  }

  Text {
    visible: root.available && root.running + root.waiting === 0
    width: root.width
    wrapMode: Text.Wrap
    textFormat: Text.PlainText
    text: root.t("card.nothing")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
  }

  Column {
    visible: root.needs.length > 0
    width: root.width
    spacing: Style.space(4)
    PanelSectionHeader { text: root.t("card.needsYou"); foreground: root.fg; fontFamily: root.fontFamily }
    Repeater {
      model: root.needs
      AgentRow {
        required property var modelData
        width: root.width
        agent: modelData
        strings: root.strings
        frame: root.frame
        animate: root.animate
        foreground: root.fg
        hot: root.hot
        hue: root.hues ? root.hues.hue(modelData.family) : root.dim
        fontFamily: root.fontFamily
      }
    }
  }

  Column {
    visible: root.others.length > 0
    width: root.width
    spacing: Style.space(4)
    Item {
      width: root.width
      height: runningHeader.implicitHeight
      PanelSectionHeader { id: runningHeader; text: root.t("card.running"); foreground: root.fg; fontFamily: root.fontFamily }
      PanelSectionHeader { anchors.right: parent.right; text: root.n(root.running); foreground: root.fg; fontFamily: root.fontFamily }
    }
    Repeater {
      model: root.others
      AgentRow {
        required property var modelData
        width: root.width
        agent: modelData
        strings: root.strings
        frame: root.frame
        animate: root.animate
        foreground: root.fg
        hot: root.hot
        hue: root.hues ? root.hues.hue(modelData.family) : root.dim
        fontFamily: root.fontFamily
      }
    }
  }

  Text {
    visible: text !== ""
    width: root.width
    wrapMode: Text.Wrap
    textFormat: Text.PlainText
    // Spaces inside each part are no-break ones, so a long footer wraps at the
    // separator and never leaves the last word on a line of its own.
    text: [root.more > 0 ? root.tn("card.more", root.more) : "", root.failed > 0 ? root.tn("card.failed", root.failed) : ""]
      .filter(function(s) { return s }).map(function(s) { return s.replace(/ /g, "\u00a0") }).join("\u00a0· ")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }
}
