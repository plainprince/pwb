<plain>
    <head>
        <title>Welcome to PlainWeb</title>
    </head>
    <view class="welcome">
        <component name="chip" params="label">
            <view class="chip"><text>{label}</text></view>
        </component>
        <text class="title">Welcome to PlainWeb</text>
        <text class="subtitle">A plain web — built from PML, PSS, and JS.</text>
        <view class="card">
            <text class="card-title">Native hyperlinks</text>
            <text class="card-hint">A link element navigates the browser with no JavaScript needed.</text>
            <link href="demo.plain">Open demo.plain →</link>
        </view>
        <view class="card">
            <text class="card-title">Network tools</text>
            <text class="card-hint">Search the swarm, claim a .pwb name, inspect peers, or check this node's status.</text>
            <link href="search.plain">search.plain →</link>
            <link href="register.plain">register.plain →</link>
            <link href="nodes.plain">nodes.plain →</link>
            <link href="network.plain">network.plain →</link>
            <link href="help.plain">help.plain →</link>
        </view>
        <view class="card">
            <text class="card-title">Right-click the button below</text>
            <text class="card-hint">A context menu handler shows new text and calls preventDefault() to suppress the native menu.</text>
            <button id="ctx-btn">Right-click me</button>
            <view class="chips">
                <import component="chip" label="built from components"></import>
            </view>
        </view>
    </view>
</plain>
