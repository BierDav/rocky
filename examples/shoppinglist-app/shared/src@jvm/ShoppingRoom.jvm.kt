package org.example.examples.shopping

import androidx.room.Room
import java.io.File
import rocky.RockyDatabase
import rocky.room.setRockyDatabase

/** Room on [runtime]'s own file. Room keeps only a lock file in [directory]. */
fun openShoppingDatabase(directory: File, runtime: RockyDatabase): ShoppingDatabase =
    Room.databaseBuilder<ShoppingDatabase>(File(directory, "shopping").absolutePath).setRockyDatabase(runtime).build()
