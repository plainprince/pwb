<!DOCTYPE html>
<html>
<head>
    <title>PlainWeb DevTools</title>
    <link rel="stylesheet" href="style.pss">
    <script src="app.pjs"></script>
</head>
<body>
    <div class="devtools-sidebar">
        <div class="devtools-header">
            <div class="devtools-tabs">
                <div class="tab active" id="tab-elements" data-tab="elements"><icon-layers style="width: 1.8vh; height: 1.8vh;"/></div>
                <div class="tab" id="tab-console" data-tab="console"><icon-terminal style="width: 1.8vh; height: 1.8vh;"/></div>
            </div>
            <div class="devtools-controls">
                <button id="select-element" title="Select element to inspect">Pick</button>
                <button id="close-devtools" title="Close DevTools">×</button>
            </div>
        </div>

        <div class="tab-content active" id="elements-tab">
            <div class="elements-tree" id="elements-tree"></div>
        </div>

        <div class="tab-content" id="console-tab">
            <div class="console-output" id="console-output"></div>
            <div class="console-input-row">
                <input id="console-input" placeholder="Evaluate JS — press Enter">
                <button id="console-send" title="Send">Send</button>
            </div>
        </div>

        <div class="styles-panel" id="styles-panel">
            <text class="panel-hint">Select an element to inspect its styles.</text>
        </div>
    </div>
</body>
</html>
