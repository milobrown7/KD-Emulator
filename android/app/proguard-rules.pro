# JNI entry points are looked up by name from libkd_emulator.so.
-keep class com.kdemulator.bridge.NativeEmulatorBridge { *; }
-keepclasseswithmembernames class * { native <methods>; }
