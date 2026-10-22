package org.example.examples.shopping

import androidx.room.Room
import kotlinx.coroutines.flow.first
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.dao.CategoryDao
import org.example.examples.shopping.data.db.entity.Category
import rocky.RockyDatabase
import rocky.room.setRockyDatabase
import java.io.File

/** Room on [rocky]'s own file, without following its sync: tests read after syncing. */
internal fun openShoppingDatabase(directory: File, rocky: RockyDatabase): ShoppingDatabase = Room.databaseBuilder<ShoppingDatabase>(File(directory, "shopping").absolutePath).setRockyDatabase(rocky).build()

internal suspend fun CategoryDao.all(list: Long): List<Category> = observe(list).first()
