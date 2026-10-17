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
            <text class="card-title">Right-click the button below</text>
            <text class="card-hint">A context menu handler shows new text and calls preventDefault() to suppress the native menu.</text>
            <button id="ctx-btn">Right-click me</button>
            <view class="chips">
                <import component="chip" label="built from components"></import>
            </view>
        </view>
    </view>
</plain>
