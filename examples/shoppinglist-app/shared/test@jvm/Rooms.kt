package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.flow.first
import rocky.RockyDatabase
import rocky.room.setRockyDatabase
import java.io.File

/** Room on [rocky]'s own file, without following its sync: tests read after syncing. */
internal fun openShoppingDatabase(directory: File, rocky: RockyDatabase): ShoppingDatabase = Room.databaseBuilder<ShoppingDatabase>(File(directory, "shopping").absolutePath).setRockyDatabase(rocky).build()

/** The items of [list] as they are now. */
internal suspend fun ShoppingDao.all(list: Long): List<ShoppingItem> = observe(list).first()
internal suspend fun ListDao.all(): List<ShoppingList> = observe().first()
internal suspend fun CategoryDao.all(list: Long): List<Category> = observe(list).first()
