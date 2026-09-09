package dev.bebok.mobile;

import android.os.Bundle;
import com.getcapacitor.BridgeActivity;

public class MainActivity extends BridgeActivity {
    @Override
    public void onCreate(Bundle savedInstanceState) {
        registerPlugin(EngineLauncherPlugin.class);
        super.onCreate(savedInstanceState);
    }
}
