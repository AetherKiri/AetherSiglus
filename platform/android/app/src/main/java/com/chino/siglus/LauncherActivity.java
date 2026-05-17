package com.chino.siglus;

import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.Settings;
import android.widget.Button;
import android.widget.Toast;

import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
import androidx.annotation.Nullable;
import androidx.appcompat.app.AppCompatActivity;
import androidx.appcompat.app.AlertDialog;
import androidx.recyclerview.widget.GridLayoutManager;
import androidx.recyclerview.widget.RecyclerView;

import java.io.File;
import java.util.List;

/**
 * Minimal launcher UI:
 * - Shows imported games as a scrollable grid.
 * - Import: select a directory (SAF), then record a direct path in no-copy mode.
 * - Run: taps a tile -> writes launch.json -> starts GameActivity.
 */
public final class LauncherActivity extends AppCompatActivity implements GameAdapter.Listener {

    private GameLibrary library;
    private GameAdapter adapter;

    private final ActivityResultLauncher<Uri> openTreeLauncher =
            registerForActivityResult(new ActivityResultContracts.OpenDocumentTree(), this::onImportTreeSelected);

    @Override
    protected void onCreate(@Nullable Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        setContentView(R.layout.activity_launcher);

        library = new GameLibrary(this);

        RecyclerView rv = findViewById(R.id.game_grid);
        rv.setLayoutManager(new GridLayoutManager(this, 3));
        adapter = new GameAdapter(this);
        rv.setAdapter(adapter);

        Button importBtn = findViewById(R.id.btn_import);
        importBtn.setOnClickListener(v -> startImportFlow());

        refresh();
    }

    private void refresh() {
        List<GameEntry> entries = library.load();
        adapter.setItems(entries);
    }

    private void startImportFlow() {
        if (!hasAllFilesAccess()) {
            showAllFilesAccessPrompt();
            return;
        }
        openTreeLauncher.launch(null);
    }

    private boolean hasAllFilesAccess() {
        // MANAGE_EXTERNAL_STORAGE applies on Android 11+.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            return Environment.isExternalStorageManager();
        }
        return true;
    }

    private void showAllFilesAccessPrompt() {
        new AlertDialog.Builder(this)
                .setTitle("Grant Storage Access")
                .setMessage("No-copy import requires 'All files access'. Please grant it, then import again.")
                .setPositiveButton("Open Settings", (d, w) -> {
                    try {
                        Intent i = new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION);
                        i.setData(Uri.parse("package:" + getPackageName()));
                        startActivity(i);
                    } catch (Throwable t) {
                        // Fallback to generic all-files access settings.
                        Intent i = new Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION);
                        startActivity(i);
                    }
                })
                .setNegativeButton("Cancel", null)
                .show();
    }

    private void onImportTreeSelected(@Nullable Uri treeUri) {
        if (treeUri == null) {
            return;
        }

        // Persist permission so we can re-import / retry if needed.
        final int flags = Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION;
        try {
            getContentResolver().takePersistableUriPermission(treeUri, flags);
        } catch (Throwable ignored) {
            // Some providers may not support persistable perms; direct-path import may still work.
        }

        Toast.makeText(this, "Importing...", Toast.LENGTH_SHORT).show();

        new Thread(() -> {
            try {
                GameLibrary.ImportedGameDraft draft = library.importFromTreeUri(treeUri);
                runOnUiThread(() -> addImportedGame(draft));
            } catch (Throwable t) {
                library.cleanupPartialImport();
                runOnUiThread(() -> Toast.makeText(this, "Import failed: " + t.getMessage(), Toast.LENGTH_LONG).show());
            }
        }, "siglus-import").start();
    }

    private void addImportedGame(GameLibrary.ImportedGameDraft draft) {
        if (draft == null) return;
        try {
            GameEntry e = library.addImportedGame(draft);
            Toast.makeText(this, "Imported: " + e.title, Toast.LENGTH_SHORT).show();
            refresh();
        } catch (Throwable t) {
            library.cleanupPartialImport();
            Toast.makeText(this, "Import failed: " + t.getMessage(), Toast.LENGTH_LONG).show();
        }
    }

    @Override
    public void onGameClicked(GameEntry e) {
        try {
            // Write launch config that native code can read on startup.
            LaunchConfig.write(this, e.rootPath);

            Intent it = new Intent(this, SiglusGameActivity.class);
            startActivity(it);
        } catch (Throwable t) {
            Toast.makeText(this, "Failed to start: " + t.getMessage(), Toast.LENGTH_LONG).show();
        }
    }

    @Override
    public void onGameLongPressed(GameEntry e) {
        if (e == null) return;

        new AlertDialog.Builder(this)
                .setTitle(e.title)
                .setItems(new String[] { "Remove" }, (d, which) -> {
                    Toast.makeText(this, "Use the launcher list controls to remove games.", Toast.LENGTH_SHORT).show();
                })
                .setNegativeButton("Cancel", null)
                .show();
    }
}
