package com.chino.siglus;

import androidx.annotation.Nullable;

public final class GameEntry {
    public final String id;
    public final String title;
    public final String rootPath;
    public final long addedAtEpochMs;
    @Nullable public final String coverPath;

    public GameEntry(String id, String title, String rootPath, long addedAtEpochMs, @Nullable String coverPath) {
        this.id = id;
        this.title = title;
        this.rootPath = rootPath;
        this.addedAtEpochMs = addedAtEpochMs;
        this.coverPath = coverPath;
    }
}
