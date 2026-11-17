import QtQuick
import qs.Commons
import qs.Ui

// The card a click on the mark opens: a KeyboardPanel, so it takes keyboard
// focus as it opens (Escape closes it; Enter opens the dashboard), as
// Omarchy's own Agents panel does. A PopupCard would not get keys.
//
// The bar widget loads this file through a Loader rather than declaring the
// card itself. Omarchy documents requestPopout for plugins but not these panel
// parts, so if an update renames or drops one, only this file fails to load and
// the bar mark keeps working; a click then opens the dashboard instead.
Item {
  id: card
  // The bar widget: its status, its frame, cardOpen, close() and markButton.
  property var widget: null

  KeyboardPanel {
    id: panel
    anchorItem: card.widget ? card.widget.markButton : null
    bar: card.widget ? card.widget.bar : null
    // Closing goes through the widget, which owns cardOpen; the bar also calls
    // the owner's close() when another popout opens.
    owner: card.widget
    open: !!card.widget && card.widget.cardOpen
    focusTarget: panel.loadedContent
    contentWidth: panel.fittedContentWidth(Style.space(380))
    contentHeight: panel.fittedContentHeight(panel.loadedContentHeight)

    // Built the first time the card opens and kept after, so a closed card that
    // was never opened costs nothing.
    //
    // Inside this component a property named like an id binds to itself:
    // `hues: hues` on the Roster would read the Roster's own (empty) property and
    // silently drop the provider colors. Every id here is named apart from the
    // properties it feeds.
    content: PanelKeyCatcher {
      implicitHeight: roster.implicitHeight
      anchors.fill: parent
      onCloseRequested: card.widget.close()
      onActivateRequested: card.widget.openDashboardFromCard()

      Hues { id: themeHues }

      // Reread the theme's hues on every open, so a theme switched while the
      // card was closed shows right away.
      Connections {
        target: card.widget
        function onCardOpenChanged() { if (card.widget.cardOpen) themeHues.reload() }
      }

      Roster {
        id: roster
        width: parent.width
        status: card.widget.status
        strings: card.widget.strings
        reason: card.widget.broken ? card.widget.reason() : ""
        frame: card.widget.frame
        animate: card.widget.animate
        hues: themeHues
        fontFamily: card.widget.bar ? card.widget.bar.fontFamily : Style.font.family
        onOpenDashboard: card.widget.openDashboardFromCard()
      }
    }
  }
}
