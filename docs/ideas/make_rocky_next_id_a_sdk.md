currently we use `SELECT rocky_next_id('lists')` to get the next id, actually I what a neat kotlin function and not a dao call, this makes the code cleaner

maybe something like `Rocky.nextId<List>()` or so

furthermore we need to use our sqlite function as a default value for the id column, when id would be 0 or null
