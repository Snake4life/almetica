use postgres::GenericClient;

/// Handles the user of a player (the characters).
use crate::Result;

/// Creates a new user.
pub fn create<C>(mut _conn: C) -> Result<()>
where
    C: GenericClient,
{
    Ok(())
}

/// Updates a user.
pub fn update<C>(mut _conn: C) -> Result<()>
where
    C: GenericClient,
{
    Ok(())
}

/// Finds a user by ID.
pub fn get_by_id<C>(mut _conn: C, _id: u64) -> Result<()>
where
    C: GenericClient,
{
    Ok(())
}

/// Deletes a user with the given ID.
pub fn delete<C>(mut _conn: C, _id: u64) -> Result<()>
where
    C: GenericClient,
{
    Ok(())
}
